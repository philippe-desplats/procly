mod common;

use std::net::TcpStream;
use std::path::Path;
use std::time::Duration;

use common::{Procly, assert_success, free_port, git_repository, stderr, stdout, test_server};

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

fn listens(port: u16) -> bool {
    TcpStream::connect(("127.0.0.1", port)).is_ok()
}

#[test]
fn up_starts_the_needs_first_and_waits_for_them_to_be_healthy() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let (db, api, web) = (free_port(), free_port(), free_port());
    let order = procly.scratch().join("order");
    let server = test_server();
    // The db listens a second after it started, so web only comes after it if `up` waited for its health.
    let config = format!(
        "[services.web]\nrun = 'echo web >> {order}; exec {server} --port {web}'\nports = {{ http = {web} }}\n\
         needs = ['api', 'db']\n\
         [services.api]\nrun = 'echo api >> {order}; exec {server} --port {api}'\nports = {{ http = {api} }}\n\
         needs = ['db']\n\
         [services.db]\nrun = 'sleep 1; echo db >> {order}; exec {server} --port {db}'\nports = {{ http = {db} }}\n",
        order = order.display(),
        server = server.display()
    );
    write(&main, "procly.toml", &config);

    let output = procly.procly_in(&main, &["up", "web"]);

    assert_success(&output);
    // Only the requested Service answers on stdout, its needs are told on stderr.
    assert_eq!(
        stdout(&output),
        format!("web started http://localhost:{web} port {web}\n")
    );
    assert_eq!(
        stderr(&output),
        format!(
            "db started http://localhost:{db} port {db} (needed by web)\n\
             api started http://localhost:{api} port {api} (needed by web)\n"
        )
    );
    assert_eq!(lines(&order), ["db", "api", "web"]);
    assert!(listens(db) && listens(api) && listens(web));

    // A need that already runs is reused, and a Service without needs prints nothing on stderr.
    let output = procly.procly_in(&main, &["up", "web"]);
    assert_success(&output);
    assert_eq!(
        stderr(&output),
        format!(
            "db reused http://localhost:{db} port {db} (needed by web)\n\
             api reused http://localhost:{api} port {api} (needed by web)\n"
        )
    );
    assert_eq!(lines(&order), ["db", "api", "web"]);
}

#[test]
fn up_stops_at_a_need_that_never_becomes_healthy_and_does_not_start_the_service() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let started = procly.scratch().join("started");
    let config = format!(
        "[services.web]\nrun = 'echo web >> {started}'\nneeds = ['db']\n\
         [services.db]\nrun = 'echo cannot start >&2; exit 7'\nports = {{ http = {} }}\n",
        free_port(),
        started = started.display()
    );
    write(&main, "procly.toml", &config);

    let output = procly.procly_in(&main, &["up", "web"]);

    assert_eq!(output.status.code(), Some(3), "stderr: {}", stderr(&output));
    assert_eq!(stdout(&output), "");
    assert_eq!(
        stderr(&output),
        "procly: service db, needed by web, exited with code 7 before it became healthy, its recent output:\n\
         cannot start\n"
    );
    assert!(!started.exists(), "web started although db never became healthy");
}

#[test]
fn up_refuses_needs_that_are_not_declared_or_that_loop_back() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    write(&main, "procly.toml", "[services.web]\nrun = 'true'\nneeds = ['db']\n");

    let output = procly.procly_in(&main, &["up", "web"]);
    assert_eq!(output.status.code(), Some(125));
    assert!(
        stderr(&output).contains("service web needs db, which is not declared"),
        "{}",
        stderr(&output)
    );

    let config = "[services.web]\nrun = 'true'\nneeds = ['api']\n\
                  [services.api]\nrun = 'true'\nneeds = ['web']\n";
    write(&main, "procly.toml", config);
    let output = procly.procly_in(&main, &["up", "web"]);
    assert_eq!(output.status.code(), Some(125));
    assert!(
        stderr(&output).contains("services need each other in a loop: web -> api -> web"),
        "{}",
        stderr(&output)
    );
}

fn start_count(starts: &Path) -> usize {
    lines(starts).len()
}

/// A `procly.toml` declaring `web`, which serves on its first start and exits at once on every later one, with
/// `restart` as the content of its `[services.web.restart]` table.
fn fragile_web_on(port: u16, starts: &Path, restart: &str) -> String {
    format!(
        "[services.web]\nrun = 'echo start >> {starts}; if [ $(wc -l < {starts}) -gt 1 ]; then exit 1; fi; \
         exec {server} --port {port}'\nports = {{ http = {port} }}\n[services.web.restart]\n{restart}",
        starts = starts.display(),
        server = test_server().display()
    )
}

/// Makes the test server of `web` exit with code 1, as a Service that crashes while it runs.
fn crash(port: u16) {
    common::http_get(port, "/exit/1");
}

#[test]
fn a_crashed_service_restarts_with_increasing_delays_then_ends_failed() {
    let procly = Procly::with_clock();
    let main = git_repository(procly.scratch(), "app");
    let port = free_port();
    let starts = procly.scratch().join("starts");
    write(
        &main,
        "procly.toml",
        &fragile_web_on(port, &starts, "attempts = 2\ndelay = '1s'\n"),
    );
    assert_success(&procly.procly_in(&main, &["up", "web"]));
    crash(port);
    common::eventually("the daemon to plan the first restart", || {
        procly.daemon_log().contains("restarting in 1s (restart 1 of 2)")
    });
    // An `up` asked during the backoff waits for the Service, and learns how it ended.
    let up = procly
        .command()
        .current_dir(&main)
        .args(["up", "web"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("procly up starts");

    // The delays are about the clock of the Daemon, which stands still: nothing restarts by itself.
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(start_count(&starts), 1);
    procly.advance_clock(Duration::from_secs(1));
    common::eventually("the first restart", || start_count(&starts) == 2);

    // The restarted process exits at once, so the next delay doubles.
    common::eventually("the daemon to plan the second restart", || {
        procly.daemon_log().contains("restarting in 2s (restart 2 of 2)")
    });
    procly.advance_clock(Duration::from_secs(1));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(start_count(&starts), 2, "restarted after 1s of a 2s delay");
    procly.advance_clock(Duration::from_secs(1));
    common::eventually("the second restart", || start_count(&starts) == 3);

    common::eventually("the daemon to give up", || {
        procly.daemon_log().contains("gave up after 2 restarts")
    });
    let output = up.wait_with_output().expect("procly up ends");
    assert_eq!(output.status.code(), Some(3), "stderr: {}", stderr(&output));
    assert!(
        stderr(&output)
            .contains("procly: service web kept crashing, it was restarted 2 times and is not restarted again"),
        "{}",
        stderr(&output)
    );
    procly.advance_clock(Duration::from_secs(3600));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(start_count(&starts), 3, "restarted after giving up");

    // The failed Service is gone, the next `up` is a start of its own and gets no restart either.
    let output = procly.procly_in(&main, &["up", "web"]);
    assert_eq!(output.status.code(), Some(3), "stderr: {}", stderr(&output));
    assert!(
        stderr(&output).contains("service web exited with code 1 before it became healthy"),
        "{}",
        stderr(&output)
    );
    assert_eq!(start_count(&starts), 4);
}

#[test]
fn down_during_a_backoff_cancels_the_restart() {
    let procly = Procly::with_clock();
    let main = git_repository(procly.scratch(), "app");
    let port = free_port();
    let starts = procly.scratch().join("starts");
    write(&main, "procly.toml", &fragile_web_on(port, &starts, "delay = '1s'\n"));
    assert_success(&procly.procly_in(&main, &["up", "web"]));
    crash(port);
    common::eventually("the daemon to plan the restart", || {
        procly.daemon_log().contains("restarting in 1s")
    });

    let output = procly.procly_in(&main, &["down", "web"]);

    assert_success(&output);
    assert_eq!(stdout(&output), "web stopped\n");
    procly.advance_clock(Duration::from_secs(3600));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(start_count(&starts), 1);
}

#[test]
fn a_service_that_never_became_healthy_is_not_restarted() {
    let procly = Procly::with_clock();
    let main = git_repository(procly.scratch(), "app");
    let starts = procly.scratch().join("starts");
    let config = format!(
        "[services.web]\nrun = 'echo start >> {}; exit 7'\nports = {{ http = {} }}\n",
        starts.display(),
        free_port()
    );
    write(&main, "procly.toml", &config);

    let output = procly.procly_in(&main, &["up", "web"]);

    assert_eq!(output.status.code(), Some(3), "stderr: {}", stderr(&output));
    procly.advance_clock(Duration::from_secs(3600));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(start_count(&starts), 1);
}

/// A Service running the test server on `port`, with `extra` as more keys of its table and tables.
fn server_on(name: &str, port: u16, extra: &str) -> String {
    format!(
        "[services.{name}]\nrun = '{server} --port {port}'\nports = {{ http = {port} }}\n{extra}",
        server = test_server().display()
    )
}

/// Gives the Daemon time to take its first look at the Services that just became healthy, which it does as soon
/// as they are, so that the clock moves after it.
fn let_the_daemon_look(duration_ms: u64) {
    std::thread::sleep(Duration::from_millis(duration_ms));
}

#[test]
fn an_idle_service_stops_after_its_timeout_says_why_and_is_not_restarted() {
    let procly = Procly::with_clock();
    let main = git_repository(procly.scratch(), "app");
    let port = free_port();
    let starts = procly.scratch().join("starts");
    let config = format!(
        "[services.web]\nrun = 'echo start >> {starts}; exec {server} --port {port}'\nports = {{ http = {port} }}\n\
         [services.web.idle]\ntimeout = '10m'\n",
        starts = starts.display(),
        server = test_server().display()
    );
    write(&main, "procly.toml", &config);
    assert_success(&procly.procly_in(&main, &["up", "web"]));
    let_the_daemon_look(300);

    procly.advance_clock(Duration::from_mins(9));
    let_the_daemon_look(300);
    assert!(listens(port), "stopped before the 10 minutes of idleness");
    procly.advance_clock(Duration::from_secs(60));

    common::eventually("the idle service to stop", || !listens(port));
    common::eventually("the daemon to say why", || {
        procly
            .daemon_log()
            .contains("stopped after 10m idle: it printed nothing and used under 1% of a CPU")
    });
    let output = procly.procly_in(&main, &["down", "web"]);
    assert_eq!(stdout(&output), "web not running\n");
    // An idle stop is no crash.
    procly.advance_clock(Duration::from_secs(3600));
    let_the_daemon_look(300);
    assert_eq!(start_count(&starts), 1);
    let output = procly.procly_in(&main, &["up", "web"]);
    assert_eq!(
        stdout(&output),
        format!("web started http://localhost:{port} port {port}\n")
    );
}

#[test]
fn a_service_that_prints_or_uses_cpu_is_not_idle_while_a_silent_one_stops() {
    let procly = Procly::with_clock();
    let main = git_repository(procly.scratch(), "app");
    let (chatty, quiet) = (free_port(), free_port());
    let busy_pid = procly.scratch().join("busy.pid");
    let idle = "[services.NAME.idle]\ntimeout = '10m'\n";
    let config = format!(
        "[services.chatty]\nrun = 'while :; do echo tick; sleep 0.1; done & exec {server} --port {chatty}'\n\
         ports = {{ http = {chatty} }}\n{}\
         [services.busy]\nrun = 'echo $$ > {busy_pid}; while :; do :; done'\n{}{}",
        idle.replace("NAME", "chatty"),
        idle.replace("NAME", "busy"),
        server_on("quiet", quiet, &idle.replace("NAME", "quiet")),
        server = test_server().display(),
        busy_pid = busy_pid.display()
    );
    write(&main, "procly.toml", &config);
    for service in ["chatty", "busy", "quiet"] {
        assert_success(&procly.procly_in(&main, &["up", service]));
    }
    let busy = common::read_pid(&busy_pid);
    // The busy loop must have used a measurable share of CPU, which `ps` counts in whole seconds on Linux.
    let_the_daemon_look(2500);

    procly.advance_clock(Duration::from_mins(10));

    common::eventually("the silent service to stop", || !listens(quiet));
    let_the_daemon_look(500);
    assert!(listens(chatty), "a service that prints was stopped");
    assert!(common::is_alive(busy), "a service that uses CPU was stopped");
}

#[test]
fn idle_stop_is_set_in_the_settings_and_can_be_disabled_per_service() {
    let procly = Procly::with_clock();
    let main = git_repository(procly.scratch(), "app");
    let (kept, dropped) = (free_port(), free_port());
    std::fs::write(
        procly.config_dir().join("config.toml"),
        "[settings]\nidle_timeout = '5m'\n",
    )
    .unwrap();
    let config = format!(
        "{}{}",
        server_on("kept", kept, "[services.kept.idle]\nstop = false\n"),
        server_on("dropped", dropped, "")
    );
    write(&main, "procly.toml", &config);
    for service in ["kept", "dropped"] {
        assert_success(&procly.procly_in(&main, &["up", service]));
    }
    let_the_daemon_look(300);

    procly.advance_clock(Duration::from_mins(5));

    common::eventually("the service on the settings timeout to stop", || !listens(dropped));
    let_the_daemon_look(500);
    assert!(listens(kept), "a service with idle stop disabled was stopped");
}
