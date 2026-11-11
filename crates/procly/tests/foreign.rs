//! Foreign processes: `procly status` lists what runs in a Worktree without Procly having started it.

mod common;

use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use common::{Procly, assert_success, eventually, free_port, git_repository, git_worktree_add, stdout, test_server};

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
        let child = Command::new(test_server())
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

    assert!(status.contains(&format!("service web healthy port {port}")), "{status}");
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
    assert!(status.contains(&format!("service web healthy port {port}")), "{status}");

    assert_success(&procly.procly_in(&main, &["down", "web"]));

    let status = stdout(&procly.procly_in(&main, &["status"]));
    assert!(!status.contains("foreign"), "{status}");
}
