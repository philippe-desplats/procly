//! Foreign processes: `procly status` lists what runs in a Worktree without Procly having started it.

mod common;

use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};

use common::{Procly, assert_success, eventually, free_port, git_repository, git_worktree_add, stdout, test_server};

/// A dev process started outside Procly, as an agent bypassing it would, killed when the test ends.
struct StartedOutside {
    child: Child,
    port: u16,
}

impl StartedOutside {
    fn start(dir: &Path) -> Self {
        let port = free_port();
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
