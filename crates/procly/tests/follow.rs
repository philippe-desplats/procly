//! `procly logs --follow`: the output of a Job or a Process service streamed until it ends or Ctrl-C.

mod common;

use std::io::{BufRead as _, BufReader, Read as _};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use common::{DEADLINE, Procly, assert_success, free_port, git_repository, stderr, stdout, test_server};
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;

/// A running `procly logs --follow`, whose stdout is read line by line as it comes.
struct Following {
    child: Child,
    lines: mpsc::Receiver<String>,
}

impl Following {
    fn start(procly: &Procly, dir: &Path, args: &[&str]) -> Self {
        let mut child = procly
            .command()
            .current_dir(dir)
            .args([&["logs", "--follow"], args].concat())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("procly logs --follow starts");
        let stdout = child.stdout.take().expect("stdout is piped");
        // Bounded, so that a test that stops reading also stops the reads of stdout, as a slow reader would.
        let (sender, lines) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { return };
                if sender.send(line).is_err() {
                    return;
                }
            }
        });
        Self { child, lines }
    }

    /// The next line of stdout, failing the test when none comes in time.
    fn line(&self) -> String {
        self.lines
            .recv_timeout(DEADLINE)
            .unwrap_or_else(|_| panic!("no line from procly logs --follow within {DEADLINE:?}"))
    }

    /// The next line of stdout as a JSON document, which must carry the schema version and the name followed.
    fn event(&self, name: &str) -> serde_json::Value {
        let line = self.line();
        let event: serde_json::Value =
            serde_json::from_str(&line).unwrap_or_else(|error| panic!("not one JSON document ({error}): {line:?}"));
        assert_eq!(event["schema_version"], 1, "{line}");
        assert_eq!(event["name"], name, "{line}");
        event
    }

    /// Waits for the command to exit, returning its status, the lines it printed that were not read yet and its
    /// stderr.
    fn finish(mut self) -> (ExitStatus, Vec<String>, String) {
        let status = exit_status(&mut self.child);
        let mut errors = String::new();
        self.child
            .stderr
            .take()
            .expect("stderr is piped")
            .read_to_string(&mut errors)
            .expect("read stderr");
        let mut rest = Vec::new();
        while let Ok(line) = self.lines.recv_timeout(Duration::from_secs(1)) {
            rest.push(line);
        }
        (status, rest, errors)
    }
}

/// The exit status of `child`, failing the test when it does not exit in time.
fn exit_status(child: &mut Child) -> ExitStatus {
    let mut status = None;
    common::eventually("procly logs --follow to exit", || {
        status = child.try_wait().expect("the status of procly logs --follow");
        status.is_some()
    });
    status.expect("eventually returned once the child exited")
}

/// A Job printing `before`, then `after` once the file `release` exists.
fn released_job(procly: &Procly, release: &Path) -> String {
    procly.run(&[&format!(
        "echo before; while [ ! -e {} ]; do sleep 0.05; done; echo after",
        release.display()
    )])
}

#[test]
fn a_followed_job_prints_its_backlog_then_its_live_output_then_ended() {
    let procly = Procly::new();
    let release = procly.scratch().join("release");
    let job = released_job(&procly, &release);
    common::eventually("the job to print its first line", || {
        stdout(&procly.procly(&["logs", &job])) == "before\n"
    });

    let following = Following::start(&procly, procly.scratch(), &["--json", &job]);
    let backlog = following.event(&job);
    assert_eq!(backlog["event"], "backlog");
    assert_eq!(backlog["output"], "before\n");
    std::fs::write(&release, "").unwrap();

    let mut output = String::new();
    loop {
        let event = following.event(&job);
        match event["event"].as_str() {
            Some("output") => output.push_str(event["output"].as_str().expect("output is text")),
            Some("ended") => break,
            _ => panic!("unexpected event {event}"),
        }
    }
    assert_eq!(output, "after\n");
    let (status, rest, errors) = following.finish();
    assert!(status.success(), "{status} {errors}");
    assert!(rest.is_empty(), "lines after ended: {rest:?}");
}

#[test]
fn a_followed_job_prints_its_output_as_text_and_exits_0_at_its_end() {
    let procly = Procly::new();
    let release = procly.scratch().join("release");
    let job = released_job(&procly, &release);
    common::eventually("the job to print its first line", || {
        stdout(&procly.procly(&["logs", &job])) == "before\n"
    });

    let following = Following::start(&procly, procly.scratch(), &[&job]);
    assert_eq!(following.line(), "before");
    std::fs::write(&release, "").unwrap();
    assert_eq!(following.line(), "after");
    let (status, rest, errors) = following.finish();
    assert!(status.success(), "{status} {errors}");
    assert!(rest.is_empty(), "{rest:?}");
    assert_eq!(errors, "");
}

#[test]
fn an_ended_job_is_followed_to_its_backlog_and_its_end_at_once() {
    let procly = Procly::new();
    let job = procly.run(&["echo done"]);
    assert_success(&procly.procly(&["wait", &job]));

    let following = Following::start(&procly, procly.scratch(), &["--json", &job]);
    assert_eq!(following.event(&job)["output"], "done\n");
    assert_eq!(following.event(&job)["event"], "ended");
    let (status, _, errors) = following.finish();
    assert!(status.success(), "{status} {errors}");
}

#[test]
fn a_followed_service_goes_on_through_its_automatic_restart_and_ends_with_it() {
    let procly = Procly::with_clock();
    let main = git_repository(procly.scratch(), "app");
    let port = free_port();
    let starts = procly.scratch().join("starts");
    // Serves on its first start, and exits at once on the next one, which ends it after its only restart.
    let config = format!(
        "[services.web]\nrun = 'echo start >> {starts}; echo \"start $(grep -c . {starts})\"; \
         if [ $(wc -l < {starts}) -gt 1 ]; then exit 1; fi; exec {server} --port {port}'\n\
         ports = {{ http = {port} }}\n[services.web.restart]\nattempts = 1\ndelay = '1s'\n",
        starts = starts.display(),
        server = test_server().display()
    );
    std::fs::write(main.join("procly.toml"), config).unwrap();
    assert_success(&procly.procly_in(&main, &["up", "web"]));

    let following = Following::start(&procly, &main, &["--json", "web"]);
    let backlog = following.event("web");
    assert_eq!(backlog["event"], "backlog");
    assert!(backlog["output"].as_str().unwrap().contains("start 1"), "{backlog}");
    common::http_get(port, "/exit/1");

    let mut output = String::new();
    let mut advanced = false;
    loop {
        let event = following.event("web");
        match event["event"].as_str() {
            Some("output") => output.push_str(event["output"].as_str().expect("output is text")),
            Some("ended") => break,
            _ => panic!("unexpected event {event}"),
        }
        // The restart waits on the clock of the Daemon, which only the test moves.
        if !advanced && output.contains("restarting in 1s") {
            assert!(!output.contains("start 2"), "{output}");
            procly.advance_clock(Duration::from_secs(1));
            advanced = true;
        }
    }
    assert!(advanced, "the restart never showed in the stream: {output}");
    assert!(output.contains("start 2"), "the output of the restarted run: {output}");
    let (status, rest, errors) = following.finish();
    assert!(status.success(), "{status} {errors}");
    assert!(rest.is_empty(), "{rest:?}");
}

#[test]
fn a_reader_too_slow_is_told_what_it_missed_and_never_holds_the_daemon() {
    let procly = Procly::new();
    let release = procly.scratch().join("release");
    // About 8 MB of output once released, far more than the queue of a follower and the socket hold.
    let job = procly.run(&[&format!(
        "while [ ! -e {} ]; do sleep 0.05; done; i=0; while [ $i -lt 2000 ]; do \
         printf '%04000d\\n' $i; i=$((i + 1)); done",
        release.display()
    )]);
    let following = Following::start(&procly, procly.scratch(), &["--json", &job]);
    assert_eq!(following.event(&job)["event"], "backlog");

    // Nobody reads the stream any more: the reading thread is held by the channel no one receives from.
    std::fs::write(&release, "").unwrap();
    let wait = procly.procly(&["wait", &job]);
    assert_success(&wait);
    assert_success(&procly.procly(&["status"]));

    let mut dropped = 0;
    loop {
        let event = following.event(&job);
        match event["event"].as_str() {
            Some("output") => {}
            Some("dropped") => dropped += event["bytes"].as_u64().expect("bytes is a number"),
            Some("ended") => break,
            _ => panic!("unexpected event {event}"),
        }
    }
    assert!(dropped > 0, "nothing was dropped");
    let (status, _, errors) = following.finish();
    assert!(status.success(), "{status} {errors}");
}

#[test]
fn ctrl_c_ends_the_follow_with_0_and_no_ended_line() {
    let procly = Procly::new();
    let release = procly.scratch().join("release");
    let job = released_job(&procly, &release);
    let following = Following::start(&procly, procly.scratch(), &["--json", &job]);
    assert_eq!(following.event(&job)["event"], "backlog");

    let pid = Pid::from_raw(i32::try_from(following.child.id()).expect("a pid fits an i32"));
    kill(pid, Signal::SIGINT).expect("signal procly logs --follow");
    let (status, rest, errors) = following.finish();
    assert_eq!(status.code(), Some(0), "{status} {errors}");
    assert!(rest.iter().all(|line| !line.contains("\"ended\"")), "{rest:?}");
    // The Daemon let go of the interrupted follow, and goes on serving the Job it followed.
    std::fs::write(&release, "").unwrap();
    assert_success(&procly.procly(&["wait", &job]));
    assert_eq!(stdout(&procly.procly(&["logs", &job])), "before\nafter\n");
}

#[test]
fn following_a_compose_service_fails_with_the_docker_compose_command_to_run() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    std::fs::write(
        main.join("procly.toml"),
        format!("[services.db]\ncompose = \"db\"\nports = {{ db = {} }}\n", free_port()),
    )
    .unwrap();
    let bin = fake_docker(procly.scratch());
    // The Daemon takes the PATH of the command that starts it, and needs git.
    assert_success(&procly.procly_in(&main, &["status"]));

    let output = procly
        .command()
        .current_dir(&main)
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .args(["logs", "--follow", "db"])
        .output()
        .expect("the procly binary runs");

    assert_eq!(output.status.code(), Some(125), "{}", stderr(&output));
    assert_eq!(stdout(&output), "");
    let message = stderr(&output);
    assert!(message.contains("db is a Compose service"), "{message}");
    assert!(
        message.contains("&& docker compose --project-name acme logs -f db`"),
        "{message}"
    );
}

#[test]
fn following_a_name_that_is_neither_a_job_nor_a_service_fails_with_125() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");

    let output = procly.procly_in(&main, &["logs", "--follow", "--json", "api"]);

    assert_eq!(output.status.code(), Some(125));
    assert_eq!(stdout(&output), "");
    assert!(stderr(&output).contains("no job or service api"), "{}", stderr(&output));
}

/// A `docker` in its own directory under `dir`, which answers a compose v2 version and the stack of project `acme`.
fn fake_docker(dir: &Path) -> PathBuf {
    let bin = dir.join("bin");
    std::fs::create_dir(&bin).expect("create the bin directory");
    let docker = bin.join("docker");
    std::fs::write(
        &docker,
        "#!/bin/sh\ncase \"$*\" in\n  *version*) echo 2.30.0 ;;\n  *config*) echo '{\"name\":\"acme\"}' ;;\n  \
         *) exit 1 ;;\nesac\n",
    )
    .expect("write the fake docker");
    std::fs::set_permissions(&docker, std::fs::Permissions::from_mode(0o755)).expect("make it executable");
    bin
}
