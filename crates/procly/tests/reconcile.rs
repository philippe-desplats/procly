mod common;

use std::path::Path;
use std::time::Duration;

use nix::sys::signal::{Signal, kill, killpg};
use nix::unistd::Pid;

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

/// Like [`web_on`], for a `web` whose leader, the shell, prints a numbered tick every 50 ms for as long as it runs.
fn chatty_web_on(port: u16, starts: &Path, pid: &Path) -> String {
    format!(
        "[services.web]\nrun = '{}'\nports = {{ http = {port} }}\n",
        chatty_run(starts, pid)
    )
}

/// Kills the process group led by `0` when the test ends, passed or failed.
///
/// A test that fails while no Daemon runs leaves nobody to stop its Services, and the test server [`chatty_run`]
/// starts in the background of the shell outlives the shell, re-parented to init and still listening.
struct ReapGroup(Pid);

impl Drop for ReapGroup {
    fn drop(&mut self) {
        // Already gone in the usual case, once `down` or the Daemon stopped it.
        let _ = killpg(self.0, Signal::SIGKILL);
    }
}

/// The command of the `web` of [`chatty_web_on`].
fn chatty_run(starts: &Path, pid: &Path) -> String {
    format!(
        "echo start >> {starts}; echo $$ > {pid}; {server} & i=0; while true; do i=$((i+1)); echo tick $i; sleep 0.05; \
         done",
        starts = starts.display(),
        pid = pid.display(),
        server = test_server().display()
    )
}

#[test]
fn a_service_that_keeps_printing_after_the_daemon_was_killed_stays_alive_and_is_reused() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let port = free_port();
    let (starts, pid) = (procly.scratch().join("starts"), procly.scratch().join("pid"));
    write(&main, "procly.toml", &chatty_web_on(port, &starts, &pid));
    assert_success(&procly.procly_in(&main, &["up", "web"]));
    let leader = read_pid(&pid);
    let _group = ReapGroup(leader);

    procly.kill_daemon();
    // Long enough for several ticks to be printed while no Daemon runs.
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        common::is_alive(leader),
        "the service died printing after the daemon crashed"
    );

    let output = procly.procly_in(&main, &["up", "web"]);
    assert_success(&output);
    assert_eq!(
        stdout(&output),
        format!("web reused http://localhost:{port} port {port}\n")
    );
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        common::is_alive(leader),
        "the re-attached service died at its next write"
    );
    assert_eq!(lines(&starts), ["start"]);
}

/// The highest tick `logs web` prints, 0 before the first one.
fn last_tick(procly: &Procly, main: &Path) -> u64 {
    let output = procly.procly_in(main, &["logs", "web"]);
    assert_success(&output);
    stdout(&output)
        .lines()
        .filter_map(|line| line.strip_prefix("tick ")?.parse().ok())
        .max()
        .unwrap_or(0)
}

#[test]
fn logs_of_a_reattached_service_show_its_output_from_before_and_after_the_daemon_restart() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let (starts, pid) = (procly.scratch().join("starts"), procly.scratch().join("pid"));
    write(&main, "procly.toml", &chatty_web_on(free_port(), &starts, &pid));
    assert_success(&procly.procly_in(&main, &["up", "web"]));
    let _group = ReapGroup(read_pid(&pid));
    common::eventually("a few ticks", || last_tick(&procly, &main) >= 3);
    let before = last_tick(&procly, &main);

    procly.kill_daemon();
    std::thread::sleep(Duration::from_millis(300));

    common::eventually("ticks printed after the restart", || {
        last_tick(&procly, &main) >= before + 10
    });
    let logs = stdout(&procly.procly_in(&main, &["logs", "web"]));
    assert!(logs.lines().any(|line| line == "tick 1"), "{logs}");
}

#[test]
fn logs_of_a_reattached_service_keep_the_output_from_before_the_daemon_last_emptied_its_output_file() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let port = free_port();
    let (starts, pid) = (procly.scratch().join("starts"), procly.scratch().join("pid"));
    // A burst of a megabyte and a half, which the Daemon empties once it has read it, with a marker in its last
    // megabyte, the part the Daemon keeps.
    let filler = procly.scratch().join("filler");
    std::fs::write(&filler, "procly-filler\n".repeat(512 * 1024 / 14)).expect("write the filler");
    let run = format!(
        "cat {filler}; cat {filler}; echo procly-marker; cat {filler}; {}",
        chatty_run(&starts, &pid),
        filler = filler.display()
    );
    write(
        &main,
        "procly.toml",
        &format!("[services.web]\nrun = '{run}'\nports = {{ http = {port} }}\n"),
    );
    assert_success(&procly.procly_in(&main, &["up", "web"]));
    let _group = ReapGroup(read_pid(&pid));
    common::eventually("a few ticks", || last_tick(&procly, &main) >= 3);

    procly.kill_daemon();
    assert_success(&procly.procly_in(&main, &["up", "web"]));

    let logs = stdout(&procly.procly_in(&main, &["logs", "web"]));
    assert!(logs.contains("procly-marker\n"), "the marker was lost with the restart");
}

/// The output files the Daemon holds in the state directory.
fn output_files(procly: &Procly) -> usize {
    std::fs::read_dir(procly.state_dir().join("output")).map_or(0, Iterator::count)
}

#[test]
fn the_output_file_of_a_reattached_service_is_removed_once_it_is_stopped() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let (starts, pid) = (procly.scratch().join("starts"), procly.scratch().join("pid"));
    write(&main, "procly.toml", &chatty_web_on(free_port(), &starts, &pid));
    assert_success(&procly.procly_in(&main, &["up", "web"]));
    let _group = ReapGroup(read_pid(&pid));
    procly.kill_daemon();
    assert_success(&procly.procly_in(&main, &["up", "web"]));
    assert_eq!(output_files(&procly), 1);

    assert_success(&procly.procly_in(&main, &["down", "web"]));

    assert_eq!(output_files(&procly), 0);
}

#[test]
fn the_output_file_of_a_service_that_ended_while_no_daemon_ran_is_removed_by_the_next_one() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let (starts, pid) = (procly.scratch().join("starts"), procly.scratch().join("pid"));
    write(&main, "procly.toml", &web_on(free_port(), &starts, &pid));
    assert_success(&procly.procly_in(&main, &["up", "web"]));
    let server = read_pid(&pid);
    procly.kill_daemon();
    kill(server, Signal::SIGKILL).expect("kill the server while no Daemon runs");
    common::wait_until_gone(server);
    assert_eq!(output_files(&procly), 1);

    assert_success(&procly.procly_in(&main, &["status"]));

    assert_eq!(output_files(&procly), 0);
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
fn an_unreadable_service_document_fails_up_with_125_naming_it_until_it_is_removed() {
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

    // Removing the document is enough, the Daemon reconciles again on the next request.
    std::fs::remove_file(&document).expect("remove the document");
    assert_success(&procly.procly_in(&main, &["up", "web"]));
    assert_eq!(lines(&starts), ["start"]);
}
