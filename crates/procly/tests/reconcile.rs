mod common;

use std::path::Path;

use common::{Procly, assert_success, free_port, git_repository, read_pid, stdout, test_server};

/// Writes `content` to `name` at the root of the Worktree `dir`.
fn write(dir: &Path, name: &str, content: &str) {
    std::fs::write(dir.join(name), content).expect("write the config file");
}

fn lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// A `procly.toml` declaring `web` on `port`, which counts its starts in `starts` and writes the pid of its test
/// server, the leader of its process group, in `pid`.
fn web_on(port: u16, starts: &Path, pid: &Path) -> String {
    format!(
        "[services.web]\nrun = 'echo start >> {starts}; echo $$ > {pid}; exec {server}'\nports = {{ http = {port} }}\n",
        starts = starts.display(),
        pid = pid.display(),
        server = test_server().display()
    )
}

#[test]
fn a_service_running_when_the_daemon_was_killed_is_reattached_and_reused_not_started_twice() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let port = free_port();
    let (starts, pid) = (procly.scratch().join("starts"), procly.scratch().join("pid"));
    write(&main, "procly.toml", &web_on(port, &starts, &pid));
    assert_success(&procly.procly_in(&main, &["up", "web"]));
    let server = read_pid(&pid);

    procly.kill_daemon();
    assert!(common::is_alive(server), "the service died with the daemon");

    let output = procly.procly_in(&main, &["up", "web"]);
    assert_success(&output);
    assert_eq!(
        stdout(&output),
        format!("web reused http://localhost:{port} port {port}\n")
    );
    assert_eq!(lines(&starts), ["start"]);
    assert!(common::is_alive(server));
}
