mod common;

use std::net::TcpStream;
use std::path::Path;
use std::process::Stdio;

use common::{Procly, assert_success, eventually, free_port, git_repository, stderr, stdout, test_server};
use serde_json::Value;

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

/// Runs `procly <args>` from `dir` with `GREETING` set to `greeting` in its environment.
fn greeting(procly: &Procly, dir: &Path, greeting: &str, args: &[&str]) -> std::process::Output {
    procly
        .command()
        .current_dir(dir)
        .env("GREETING", greeting)
        .args(args)
        .output()
        .expect("the procly binary runs")
}

/// The raw text of `procly status --json` in `dir`.
fn status_text(procly: &Procly, dir: &Path) -> String {
    let output = procly.procly_in(dir, &["status", "--json"]);
    assert_success(&output);
    stdout(&output)
}

/// The status of the Service `service` of the Worktree `dir`, from `procly status --json`.
fn service_status(procly: &Procly, dir: &Path, service: &str) -> Value {
    let status: Value = serde_json::from_str(&status_text(procly, dir)).expect("a JSON status");
    status["repositories"][0]["worktrees"][0]["services"]
        .as_array()
        .expect("a list of Services")
        .iter()
        .find(|status| status["service"] == service)
        .unwrap_or_else(|| panic!("no Service {service} in {status}"))
        .clone()
}

/// A Service `web` that prints the `GREETING` it was started with, then serves on its Lease.
fn greeting_web(main: &Path, web: u16) {
    let server = test_server();
    write(
        main,
        "procly.toml",
        &format!(
            "[services.web]\nrun = 'echo \"greeting=$GREETING\"; exec {server} --port {{ports.http}}'\n\
             ports = {{ http = {web} }}\n",
            server = server.display()
        ),
    );
}

#[test]
fn a_restart_replays_the_environment_of_the_last_up_on_the_same_lease_with_a_new_start_time() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    greeting_web(&main, free_port());
    assert_success(&greeting(&procly, &main, "first", &["up", "web"]));
    let before = service_status(&procly, &main, "web");
    assert_eq!(before["restartable"], Value::Bool(true), "{before}");
    let port = before["port"].as_u64().expect("a Lease");

    let output = greeting(&procly, &main, "second", &["restart", "web"]);

    assert_success(&output);
    assert_eq!(
        stdout(&output),
        format!("web started http://localhost:{port} port {port}\n")
    );
    let after = service_status(&procly, &main, "web");
    assert_eq!(after["port"], before["port"], "{after}");
    assert_eq!(after["state"], "healthy", "{after}");
    assert!(
        after["started_at"].as_u64() > before["started_at"].as_u64(),
        "{before} then {after}"
    );
    // The output of the run the Restart started, which saw the environment of the `up`, not the one of its caller.
    let logs = stdout(&procly.procly_in(&main, &["logs", "web"]));
    let greetings: Vec<&str> = logs.lines().filter(|line| line.starts_with("greeting=")).collect();
    assert_eq!(greetings, ["greeting=first"], "{logs}");
    // The held environment may carry secrets, the status never tells it.
    assert!(!status_text(&procly, &main).contains("first"));
}

#[test]
fn restart_json_is_the_document_of_an_up_of_one_service() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    greeting_web(&main, free_port());
    assert_success(&procly.procly_in(&main, &["up", "web"]));

    let output = procly.procly_in(&main, &["restart", "--json", "web"]);

    assert_success(&output);
    let document = common::json_document(&output);
    assert_eq!(document["result"], "healthy", "{document}");
    let services = document["services"].as_array().expect("the Services reached");
    assert_eq!(services.len(), 1, "{document}");
    assert_eq!(services[0]["service"], "web", "{document}");
    assert_eq!(services[0]["outcome"], "started", "{document}");
}

#[test]
fn a_restart_starts_the_needs_that_do_not_run_and_leaves_the_running_ones_alone() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let order = procly.scratch().join("order");
    let server = test_server();
    let service = |name: &str, needs: &str| {
        format!(
            "[services.{name}]\nrun = 'echo {name} >> {order}; exec {server} --port {{ports.http}}'\n\
             ports = {{ http = {port} }}\n{needs}",
            order = order.display(),
            server = server.display(),
            port = free_port(),
        )
    };
    let config = [
        service("web", "needs = ['api', 'db']\n"),
        service("api", "needs = ['db']\n"),
        service("db", ""),
    ]
    .concat();
    write(&main, "procly.toml", &config);
    assert_success(&procly.procly_in(&main, &["up", "web"]));
    assert_success(&procly.procly_in(&main, &["down", "api"]));
    let db_started = service_status(&procly, &main, "db")["started_at"].clone();

    let output = procly.procly_in(&main, &["restart", "web"]);

    assert_success(&output);
    let needs: Vec<String> = stderr(&output)
        .lines()
        .map(|line| line.split(' ').take(2).collect::<Vec<_>>().join(" "))
        .collect();
    assert_eq!(needs, ["db reused", "api started"], "{}", stderr(&output));
    assert_eq!(lines(&order), ["db", "api", "web", "api", "web"]);
    assert_eq!(service_status(&procly, &main, "db")["started_at"], db_started);
}

#[test]
fn a_reattached_service_has_no_environment_to_restart_with_and_is_left_running() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    greeting_web(&main, free_port());
    assert_success(&procly.procly_in(&main, &["up", "web"]));
    procly.kill_daemon();
    let reattached = service_status(&procly, &main, "web");
    assert_eq!(reattached["reattached"], Value::Bool(true), "{reattached}");
    assert_eq!(reattached["restartable"], Value::Bool(false), "{reattached}");

    let output = procly.procly_in(&main, &["restart", "web"]);

    assert_eq!(output.status.code(), Some(125));
    assert_eq!(
        stderr(&output),
        "procly: web has no environment to restart with, run procly up web from a shell in the Worktree\n"
    );
    let after = service_status(&procly, &main, "web");
    assert_eq!(after["state"], "healthy", "{after}");
    assert_eq!(after["started_at"], reattached["started_at"], "{after}");
}

#[test]
fn the_worktree_release_drops_the_held_environment() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    greeting_web(&main, free_port());
    assert_success(&procly.procly_in(&main, &["up", "web"]));

    assert_success(&procly.procly_in(&main, &["worktree", "release"]));

    let output = procly.procly_in(&main, &["restart", "web"]);
    assert_eq!(output.status.code(), Some(125), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("web has no environment to restart with"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn a_service_a_group_covers_is_restarted_through_its_group() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let starts = procly.scratch().join("starts");
    let server = test_server();
    write(
        &main,
        "procly.toml",
        &format!(
            "[services.web]\nrun = '{server} --port {{ports.http}}'\nports = {{ http = {web} }}\n\
             [services.dev]\ncovers = ['web']\n\
             run = 'echo start >> {starts}; exec {server} --port $PORT_WEB_HTTP'\n",
            server = server.display(),
            starts = starts.display(),
            web = free_port(),
        ),
    );
    assert_success(&procly.procly_in(&main, &["up", "dev"]));

    let output = procly.procly_in(&main, &["restart", "web"]);

    assert_success(&output);
    assert!(stdout(&output).starts_with("dev started"), "{}", stdout(&output));
    assert_eq!(lines(&starts), ["start", "start"]);
}

#[test]
fn a_service_that_never_became_healthy_can_be_restarted_once_its_cause_is_fixed() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let ready = procly.scratch().join("ready");
    let server = test_server();
    write(
        &main,
        "procly.toml",
        &format!(
            "[services.web]\nrun = 'test -f {ready} || exit 7; exec {server} --port {{ports.http}}'\n\
             ports = {{ http = {web} }}\n",
            ready = ready.display(),
            server = server.display(),
            web = free_port(),
        ),
    );
    assert_eq!(procly.procly_in(&main, &["up", "web"]).status.code(), Some(3));
    std::fs::write(&ready, "").expect("write the flag");

    let output = procly.procly_in(&main, &["restart", "web"]);

    assert_success(&output);
    assert!(stdout(&output).starts_with("web started"), "{}", stdout(&output));
}

#[test]
fn a_group_a_previous_daemon_started_is_restarted_with_the_environment_of_an_up_of_a_service_it_covers() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let starts = procly.scratch().join("starts");
    let server = test_server();
    write(
        &main,
        "procly.toml",
        &format!(
            "[services.web]\nrun = '{server} --port {{ports.http}}'\nports = {{ http = {web} }}\n\
             [services.dev]\ncovers = ['web']\n\
             run = 'echo start >> {starts}; exec {server} --port $PORT_WEB_HTTP'\n",
            server = server.display(),
            starts = starts.display(),
            web = free_port(),
        ),
    );
    assert_success(&procly.procly_in(&main, &["up", "dev"]));
    procly.kill_daemon();
    let reused = procly.procly_in(&main, &["up", "web"]);
    assert_success(&reused);
    assert!(stdout(&reused).contains("reused (via dev)"), "{}", stdout(&reused));

    let output = procly.procly_in(&main, &["restart", "web"]);

    assert_success(&output);
    assert!(stdout(&output).starts_with("dev started"), "{}", stdout(&output));
    assert_eq!(lines(&starts), ["start", "start"]);
}

#[test]
fn a_worktree_release_during_a_restart_wins_and_the_restart_starts_nothing_again() {
    let procly = Procly::new();
    // A Worktree other than the Main worktree, whose Lease comes from a Range of its own.
    procly.narrow_range();
    let main = git_repository(procly.scratch(), "app");
    let server = test_server();
    // The shell outlives the stop of its group by a second and a half, so the Restart waits that long in its `down`.
    write(
        &main,
        "procly.toml",
        &format!(
            "[services.web]\nrun = \"trap 'sleep 1.5; exit 0' TERM; {server} --port {{ports.http}} & wait\"\n\
             ports = {{ http = {web} }}\n",
            server = server.display(),
            web = free_port(),
        ),
    );
    common::git(&main, &["add", "."]);
    common::git(&main, &["commit", "--quiet", "--message", "procly"]);
    let feature = common::git_worktree_add(&main, "app-feature");
    assert_success(&procly.procly_in(&feature, &["up", "web"]));
    let port = service_status(&procly, &feature, "web")["port"]
        .as_u64()
        .expect("a Lease");
    let restart = procly
        .command()
        .current_dir(&feature)
        .args(["restart", "web"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the procly binary runs");
    // Logged once the Restart has read the environment it would start the Service with.
    eventually("the restart to be under way", || {
        procly.daemon_log().contains("restart web of")
    });

    let release = procly.procly_in(&feature, &["worktree", "release", "--json"]);

    assert_success(&release);
    assert_eq!(
        common::json_document(&release)["released"]["leases"][0]["port"].as_u64(),
        Some(port),
        "{}",
        stdout(&release)
    );
    let restart = restart.wait_with_output().expect("the restart ends");
    assert_eq!(restart.status.code(), Some(125), "{}", stdout(&restart));
    assert!(
        stderr(&restart).contains("was released while web was restarting"),
        "{}",
        stderr(&restart)
    );
    assert!(TcpStream::connect(("127.0.0.1", u16::try_from(port).unwrap())).is_err());
    // Nothing the Restart did outlives the release: a second release finds no Service and no Lease.
    let again = procly.procly_in(&feature, &["worktree", "release", "--json"]);
    assert_success(&again);
    let again = common::json_document(&again);
    assert_eq!(again["stopped"]["services"], Value::Array(Vec::new()), "{again}");
    assert_eq!(again["released"]["leases"], Value::Array(Vec::new()), "{again}");
}
