mod common;

use std::path::Path;

use nix::sys::signal::{Signal, kill};

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

#[test]
fn down_stops_a_reattached_service_with_its_original_process() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let port = free_port();
    let (starts, pid) = (procly.scratch().join("starts"), procly.scratch().join("pid"));
    write(&main, "procly.toml", &web_on(port, &starts, &pid));
    assert_success(&procly.procly_in(&main, &["up", "web"]));
    let server = read_pid(&pid);
    procly.kill_daemon();

    let output = procly.procly_in(&main, &["down", "web"]);

    assert_success(&output);
    assert_eq!(stdout(&output), "web stopped\n");
    assert!(!common::is_alive(server), "down left the re-attached process running");
}

#[test]
fn the_status_after_a_daemon_crash_tells_which_service_was_reattached_and_which_ended_meanwhile() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let (web, api) = (free_port(), free_port());
    let (starts, web_pid, api_pid) = (
        procly.scratch().join("starts"),
        procly.scratch().join("web.pid"),
        procly.scratch().join("api.pid"),
    );
    let config = format!(
        "{}[services.api]\nrun = 'echo $$ > {api_pid}; exec {server}'\nports = {{ http = {api} }}\n",
        web_on(web, &starts, &web_pid),
        api_pid = api_pid.display(),
        server = test_server().display()
    );
    write(&main, "procly.toml", &config);
    assert_success(&procly.procly_in(&main, &["up", "web"]));
    assert_success(&procly.procly_in(&main, &["up", "api"]));
    let api_server = read_pid(&api_pid);
    procly.kill_daemon();
    kill(api_server, Signal::SIGKILL).expect("kill the api while no Daemon runs");
    common::wait_until_gone(api_server);

    let output = procly.procly_in(&main, &["status"]);

    assert_success(&output);
    assert_eq!(
        stdout(&output),
        format!(
            "repository {git}\n  worktree app (main) {main}\n\
             \x20   service api ended: exit code unknown, a previous Daemon started it\n\
             \x20   service web healthy port {web}, re-attached after a restart of the Daemon\n\
             \x20   no jobs\n",
            git = main.join(".git").display(),
            main = main.display()
        )
    );
    let json = stdout(&procly.procly_in(&main, &["status", "--json"]));
    assert!(
        json.contains(&format!(
            r#"{{"service":"web","port":{web},"reattached":true,"state":"healthy"}}"#
        )),
        "{json}"
    );
    // The api that ended starts again, on the same Lease.
    let output = procly.procly_in(&main, &["up", "api"]);
    assert_success(&output);
    assert_eq!(
        stdout(&output),
        format!("api started http://localhost:{api} port {api}\n")
    );
}

#[test]
fn an_orphan_of_a_service_holding_its_lease_is_stopped_reported_and_the_lease_reused() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let port = free_port();
    let (leader, server) = (procly.scratch().join("leader"), procly.scratch().join("server"));
    // The shell leads the group and waits for its test server, which is left orphaned once the shell is killed.
    let config = format!(
        "[services.web]\nrun = 'echo $$ > {leader}; {exe} & echo $! > {server}; wait'\nports = {{ http = {port} }}\n",
        leader = leader.display(),
        server = server.display(),
        exe = test_server().display()
    );
    write(&main, "procly.toml", &config);
    assert_success(&procly.procly_in(&main, &["up", "web"]));
    let (leader_pid, server_pid) = (read_pid(&leader), read_pid(&server));
    procly.kill_daemon();
    kill(leader_pid, Signal::SIGKILL).expect("kill the leader while no Daemon runs");
    common::wait_until_gone(leader_pid);
    assert!(common::is_alive(server_pid), "the orphan holds the Lease");

    let output = procly.procly_in(&main, &["status"]);

    assert_success(&output);
    assert!(!common::is_alive(server_pid), "the orphan was left running");
    assert!(
        stdout(&output).contains(&format!(
            "    service web ended: its leader ended while no Daemon ran, the rest of process group {leader_pid} held \
             port {port} of its Lease and was stopped\n"
        )),
        "{}",
        stdout(&output)
    );
    assert!(
        procly.daemon_log().contains(&format!(
            "service web of {}, process group {leader_pid}, lost its leader while no Daemon ran, the rest of the \
             group held port {port} of its Lease and was stopped",
            main.display()
        )),
        "{}",
        procly.daemon_log()
    );
    let output = procly.procly_in(&main, &["up", "web"]);
    assert_success(&output);
    assert_eq!(
        stdout(&output),
        format!("web started http://localhost:{port} port {port}\n")
    );
}

#[test]
fn a_service_found_running_but_no_longer_listening_on_its_lease_is_stopped_not_reattached() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let port = free_port();
    let (leader, server) = (procly.scratch().join("leader"), procly.scratch().join("server"));
    // The shell outlives its test server, so the group still runs once the server is killed.
    let config = format!(
        "[services.web]\nrun = 'echo $$ > {leader}; {exe} & echo $! > {server}; while :; do sleep 1; done'\n\
         ports = {{ http = {port} }}\n",
        leader = leader.display(),
        server = server.display(),
        exe = test_server().display()
    );
    write(&main, "procly.toml", &config);
    assert_success(&procly.procly_in(&main, &["up", "web"]));
    let (leader_pid, server_pid) = (read_pid(&leader), read_pid(&server));
    procly.kill_daemon();
    kill(server_pid, Signal::SIGKILL).expect("kill the server while no Daemon runs");
    common::wait_until_gone(server_pid);

    let output = procly.procly_in(&main, &["status"]);

    assert_success(&output);
    assert!(
        !common::is_alive(leader_pid),
        "the group that no longer listens was left running"
    );
    assert!(
        stdout(&output).contains(&format!(
            "    service web ended: no longer listened on its Lease, port {port}, when the Daemon restarted, its \
             process group was stopped\n"
        )),
        "{}",
        stdout(&output)
    );
}

#[test]
fn a_daemon_stopped_cleanly_leaves_no_service_for_the_next_one_to_reconcile() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let (starts, pid) = (procly.scratch().join("starts"), procly.scratch().join("pid"));
    write(&main, "procly.toml", &web_on(free_port(), &starts, &pid));
    assert_success(&procly.procly_in(&main, &["up", "web"]));
    let server = read_pid(&pid);

    procly.stop_daemon();

    assert!(!common::is_alive(server));
    let output = procly.procly_in(&main, &["status"]);
    assert_success(&output);
    assert!(!stdout(&output).contains("service web"), "{}", stdout(&output));
}

#[test]
fn an_unreadable_service_document_fails_up_with_125_naming_it() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let (starts, pid) = (procly.scratch().join("starts"), procly.scratch().join("pid"));
    write(&main, "procly.toml", &web_on(free_port(), &starts, &pid));
    std::fs::create_dir_all(procly.state_dir()).unwrap();
    let document = procly.state_dir().join("services.json");
    std::fs::write(&document, "{ not json").unwrap();

    let output = procly.procly_in(&main, &["up", "web"]);

    assert_eq!(output.status.code(), Some(125));
    assert!(
        common::stderr(&output).contains(&format!("cannot parse {}, fix or remove it", document.display())),
        "{}",
        common::stderr(&output)
    );
    assert!(
        !starts.exists(),
        "a Service started although the previous ones could not be reconciled"
    );
}
