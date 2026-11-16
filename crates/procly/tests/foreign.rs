//! Foreign processes: `procly status` lists what runs in a Worktree without Procly having started it.

mod common;

use std::net::TcpStream;
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use common::{
    Procly, assert_success, eventually, free_port, git_repository, git_worktree_add, stderr, stdout, test_server,
};

/// How long after a Foreign process starts or stops listening `procly status` shows it, as `docs/daemon.md` states.
const SHOWN_WITHIN: Duration = Duration::from_secs(2);

/// A dev process started outside Procly, as an agent bypassing it would, killed when the test ends.
struct StartedOutside {
    child: Child,
    port: u16,
}

impl StartedOutside {
    fn start(dir: &Path) -> Self {
        Self::on(dir, free_port())
    }

    fn on(dir: &Path, port: u16) -> Self {
        Self::spawn(Command::new(test_server()), dir, port)
    }

    /// Started with `program` as its `argv[0]`, as a CLI installed as a file named after its version and run through a
    /// symbolic link of its own name is: the executable file name is not the name it was run by.
    fn named(dir: &Path, program: &str) -> Self {
        let mut command = Command::new(test_server());
        command.arg0(program);
        Self::spawn(command, dir, free_port())
    }

    fn spawn(mut command: Command, dir: &Path, port: u16) -> Self {
        let child = command
            .args(["--port", &port.to_string()])
            .current_dir(dir)
            .stdout(Stdio::null())
            .spawn()
            .expect("the test server starts");
        eventually("the dev server to listen", || {
            TcpStream::connect(("127.0.0.1", port)).is_ok()
        });
        Self { child, port }
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }
}

impl Drop for StartedOutside {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn a_dev_server_started_outside_procly_in_the_worktree_is_a_foreign_process_in_status() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let other = git_worktree_add(&main, "app-feature");
    let server = StartedOutside::start(&other);

    let output = procly.procly_in(&other, &["status"]);

    assert_success(&output);
    let status = stdout(&output);
    assert!(
        status.contains(&format!(
            "    foreign process {} procly-test-server port {}, started outside Procly\n",
            server.pid(),
            server.port
        )),
        "{status}"
    );
    // Listed under its own Worktree only.
    let everywhere = stdout(&procly.procly_in(&main, &["status", "--all"]));
    let (main_part, other_part) = everywhere
        .split_once("worktree app_feature")
        .expect("the status lists the second Worktree");
    assert!(!main_part.contains("foreign process"), "{everywhere}");
    assert!(
        other_part.contains(&format!("foreign process {}", server.pid())),
        "{everywhere}"
    );
}

#[test]
fn the_json_status_carries_the_foreign_processes_of_the_worktree() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let server = StartedOutside::start(&main);

    let output = procly.procly_in(&main, &["status", "--json"]);

    assert_success(&output);
    let json: serde_json::Value = serde_json::from_str(&stdout(&output)).expect("the status is JSON");
    let foreign = &json["repositories"][0]["worktrees"][0]["foreign"];
    assert_eq!(
        foreign,
        &serde_json::json!([{
            "kind": "process",
            "pid": server.pid(),
            "command": "procly-test-server",
            "ports": [server.port],
        }]),
        "{json}"
    );
}

#[test]
fn a_service_procly_started_is_no_foreign_process() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let port = free_port();
    std::fs::write(
        main.join("procly.toml"),
        format!(
            "[services.web]\nrun = '{} --port {{ports.http}}'\nports = {{ http = {port} }}\n",
            test_server().display()
        ),
    )
    .expect("write the config");
    assert_success(&procly.procly_in(&main, &["up", "web"]));
    // A Job too, listening from the Worktree.
    let job_port = free_port();
    assert_success(&procly.procly_in(
        &main,
        &["run", &format!("{} --port {job_port}", test_server().display())],
    ));
    eventually("the job to listen", || {
        TcpStream::connect(("127.0.0.1", job_port)).is_ok()
    });

    let status = stdout(&procly.procly_in(&main, &["status"]));

    assert!(
        status.contains(&format!("service web healthy http://localhost:{port} port {port}")),
        "{status}"
    );
    assert!(!status.contains("foreign"), "{status}");
}

/// The declaration of a Service `web` whose Lease is `port`.
fn web_declaring(port: u16) -> String {
    format!(
        "[services.web]\nrun = '{} --port {{ports.http}}'\nports = {{ http = {port} }}\n",
        test_server().display()
    )
}

/// Asks `procly status` in `dir` until `seen` holds of what it prints, and fails as soon as a status asked
/// [`SHOWN_WITHIN`] or more after `since` does not, which is the delay the documentation promises.
fn until_status(procly: &Procly, dir: &Path, since: Instant, what: &str, seen: impl Fn(&str) -> bool) {
    loop {
        let asked = Instant::now();
        let output = procly.procly_in(dir, &["status"]);
        assert_success(&output);
        let status = stdout(&output);
        if seen(&status) {
            return;
        }
        let waited = asked.duration_since(since);
        assert!(
            waited < SHOWN_WITHIN,
            "a status asked {waited:?} after the Foreign process {what} still does not tell it: {status}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_foreign_process_on_a_lease_shows_in_status_within_2_s_of_listening_and_leaves_within_2_s_of_ending() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let port = free_port();
    std::fs::write(main.join("procly.toml"), web_declaring(port)).expect("write the config");
    let env = procly.procly_in(&main, &["env", "web"]);
    assert_success(&env);
    assert!(stdout(&env).contains(&format!("PORT='{port}'")), "{}", stdout(&env));
    // The Daemon has looked once already, so that the next answers come from what it found and keeps looking for.
    let status = stdout(&procly.procly_in(&main, &["status"]));
    assert!(!status.contains("foreign"), "{status}");

    let server = StartedOutside::on(&main, port);
    let listening = Instant::now();
    let line = format!("foreign process {} procly-test-server port {port}", server.pid());

    until_status(&procly, &main, listening, "started listening", |status| {
        status.contains(&line)
    });

    drop(server);
    let ended = Instant::now();

    until_status(&procly, &main, ended, "ended", |status| !status.contains("foreign"));
}

#[test]
fn a_look_that_panics_never_breaks_status_nor_floods_the_daemon_log() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let panics = procly.scratch().join("look-panics");
    std::fs::write(&panics, "").expect("make every look panic");
    let polling = Instant::now();

    // The Daemon takes the environment of the command that starts it, and a client polls it as the app does.
    while polling.elapsed() < Duration::from_secs(4) {
        let output = procly
            .command()
            .current_dir(&main)
            .env("PROCLY_TEST_LOOK_PANICS", &panics)
            .arg("status")
            .output()
            .expect("the procly binary runs");
        assert_success(&output);
        std::thread::sleep(Duration::from_millis(50));
    }

    let log = procly.daemon_log();
    assert_eq!(log.matches("cannot look for Foreign processes").count(), 1, "{log}");
    // Looks 1 s, then 2 s, then 4 s apart: 3 in 4 s, where a look that never waited would panic thousands of times.
    assert!(log.matches("panicked").count() <= 4, "{log}");
    assert!(log.len() < 4096, "{log}");

    std::fs::remove_file(&panics).expect("let the looks succeed again");
    let server = StartedOutside::start(&main);
    let line = format!("foreign process {}", server.pid());
    eventually("the Foreign process to show once a look succeeds again", || {
        stdout(&procly.procly_in(&main, &["status"])).contains(&line)
    });
}

#[test]
fn a_service_stopped_by_down_is_no_foreign_process_in_the_next_status() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let port = free_port();
    std::fs::write(main.join("procly.toml"), web_declaring(port)).expect("write the config");
    assert_success(&procly.procly_in(&main, &["up", "web"]));
    // What the Daemon found lists the listener of the Service, which it ran while it looked.
    let status = stdout(&procly.procly_in(&main, &["status"]));
    assert!(
        status.contains(&format!("service web healthy http://localhost:{port} port {port}")),
        "{status}"
    );

    assert_success(&procly.procly_in(&main, &["down", "web"]));

    let status = stdout(&procly.procly_in(&main, &["status"]));
    assert!(!status.contains("foreign"), "{status}");
}

#[test]
fn a_foreign_process_is_named_by_its_argv_0_and_not_by_its_executable_file_name() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let server = StartedOutside::named(&main, "my-server");

    let status = stdout(&procly.procly_in(&main, &["status"]));
    let output = procly.procly_in(&main, &["status", "--json"]);

    assert!(
        status.contains(&format!(
            "    foreign process {} my-server port {}, started outside Procly\n",
            server.pid(),
            server.port
        )),
        "{status}"
    );
    assert_success(&output);
    let json: serde_json::Value = serde_json::from_str(&stdout(&output)).expect("the status is JSON");
    let foreign = &json["repositories"][0]["worktrees"][0]["foreign"][0];
    assert_eq!(foreign["pid"], server.pid(), "{json}");
    assert_eq!(foreign["command"], "my-server", "{json}");
}

/// The declaration of a Service `web` that runs `prefix` then the test server on its Lease `port`.
fn web_running(prefix: &str, port: u16) -> String {
    format!(
        "[services.web]\nrun = '{prefix}{} --port {{ports.http}}'\nports = {{ http = {port} }}\n",
        test_server().display()
    )
}

/// The directory `relative` of the Worktree `main`, created.
fn directory(main: &Path, relative: &str) -> std::path::PathBuf {
    let directory = main.join(relative);
    std::fs::create_dir_all(&directory).expect("create the directory");
    directory
}

/// The `procly: warning:` lines of what `up` printed on stderr.
fn warnings(output: &std::process::Output) -> Vec<String> {
    stderr(output)
        .lines()
        .filter(|line| line.starts_with("procly: warning: "))
        .map(str::to_owned)
        .collect()
}

#[test]
fn up_warns_of_a_foreign_process_listening_from_the_directory_the_service_cds_into_and_starts_it_anyway() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let web = directory(&main, "apps/web");
    let port = free_port();
    std::fs::write(main.join("procly.toml"), web_running("cd apps/web && ", port)).expect("write the config");
    let server = StartedOutside::start(&web);

    let output = procly.procly_in(&main, &["up", "web"]);

    assert_success(&output);
    assert!(stdout(&output).starts_with("web started"), "{}", stdout(&output));
    let warned = warnings(&output);
    assert_eq!(warned.len(), 1, "{}", stderr(&output));
    let warning = &warned[0];
    for told in [
        format!("foreign process {} procly-test-server", server.pid()),
        format!("port {}", server.port),
        format!("`kill {}`", server.pid()),
        "`procly status`".to_owned(),
    ] {
        assert!(warning.contains(&told), "{told} is not in {warning}");
    }

    assert_success(&procly.procly_in(&main, &["down", "web"]));
    let output = procly.procly_in(&main, &["up", "--json", "web"]);

    assert_success(&output);
    let document = common::json_document(&output);
    let services = document["services"].as_array().expect("the Services reached");
    assert_eq!(services[0]["outcome"], "started", "{document}");
    let json_warnings = services[0]["warnings"].as_array().expect("the warnings of web");
    assert_eq!(json_warnings.len(), 1, "{document}");
    assert_eq!(
        json_warnings[0].as_str(),
        warning.strip_prefix("procly: warning: "),
        "{document}"
    );
    assert!(warnings(&output).is_empty(), "{}", stderr(&output));
}

#[test]
fn a_foreign_process_in_another_directory_of_the_worktree_gives_no_warning() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    directory(&main, "apps/web");
    let api = directory(&main, "apps/api");
    std::fs::write(main.join("procly.toml"), web_running("cd apps/web && ", free_port())).expect("write the config");
    let _server = StartedOutside::start(&api);

    let output = procly.procly_in(&main, &["up", "web"]);

    assert_success(&output);
    assert!(warnings(&output).is_empty(), "{}", stderr(&output));
}

#[test]
fn a_service_without_cd_warns_of_a_foreign_process_listening_from_the_root_of_the_worktree_but_not_once_reused() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    std::fs::write(main.join("procly.toml"), web_running("", free_port())).expect("write the config");
    let server = StartedOutside::start(&main);

    let output = procly.procly_in(&main, &["up", "web"]);

    assert_success(&output);
    let warned = warnings(&output);
    assert_eq!(warned.len(), 1, "{}", stderr(&output));
    assert!(
        warned[0].contains(&format!("foreign process {}", server.pid())),
        "{warned:?}"
    );

    let reused = procly.procly_in(&main, &["up", "web"]);

    assert_success(&reused);
    assert!(stdout(&reused).starts_with("web reused"), "{}", stdout(&reused));
    assert!(warnings(&reused).is_empty(), "{}", stderr(&reused));
}

#[test]
fn a_foreign_process_that_rewrote_its_title_is_named_by_its_executable_and_nothing_of_the_title_shows() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    std::fs::write(main.join("procly.toml"), web_running("", free_port())).expect("write the config");
    // The title Puma gives itself, with a word of the command line that must never be shown.
    let server = StartedOutside::named(&main, "puma 6.4.0 (tcp://127.0.0.1:3000) [myapp] --secret-token=abc");

    let up = procly.procly_in(&main, &["up", "web"]);
    let status = stdout(&procly.procly_in(&main, &["status"]));
    let json = procly.procly_in(&main, &["status", "--json"]);

    assert_success(&up);
    let warned = warnings(&up);
    assert_eq!(warned.len(), 1, "{}", stderr(&up));
    assert!(
        warned[0].contains(&format!(
            "foreign process {} procly-test-server port {}",
            server.pid(),
            server.port
        )),
        "{warned:?}"
    );
    assert!(
        status.contains(&format!(
            "    foreign process {} procly-test-server port {}, started outside Procly\n",
            server.pid(),
            server.port
        )),
        "{status}"
    );
    assert_success(&json);
    let document: serde_json::Value = serde_json::from_str(&stdout(&json)).expect("the status is JSON");
    let foreign = &document["repositories"][0]["worktrees"][0]["foreign"][0];
    assert_eq!(foreign["pid"], server.pid(), "{document}");
    assert_eq!(foreign["command"], "procly-test-server", "{document}");
    for shown in [stderr(&up), status, stdout(&json)] {
        for word in ["secret-token", "myapp", "tcp://"] {
            assert!(!shown.contains(word), "{word} is shown in {shown}");
        }
    }
}

#[test]
fn up_never_warns_of_the_process_running_it_and_still_warns_of_an_unrelated_one() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    std::fs::write(main.join("procly.toml"), web_running("", free_port())).expect("write the config");
    let unrelated = StartedOutside::start(&main);
    // A coding agent listening from the root of the Worktree, as a CLI with an IDE bridge does, runs `procly up`.
    let procly_up = procly.command();
    let mut agent = Command::new(test_server());
    for (key, value) in procly_up.get_envs() {
        match value {
            Some(value) => agent.env(key, value),
            None => agent.env_remove(key),
        };
    }
    let agent = agent
        .current_dir(&main)
        .args(["--port", &free_port().to_string(), "--then"])
        .arg(procly_up.get_program())
        .args(["up", "web"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the test server starts");
    let agent_pid = agent.id();

    // The test server ends once `procly up` has.
    let output = agent.wait_with_output().expect("procly up runs under the test server");

    assert_success(&output);
    let warned = warnings(&output);
    assert_eq!(warned.len(), 1, "{}", stderr(&output));
    assert!(
        warned[0].contains(&format!("foreign process {} ", unrelated.pid())),
        "{warned:?}"
    );
    assert!(
        !stderr(&output).contains(&format!("foreign process {agent_pid} ")),
        "{}",
        stderr(&output)
    );
}
