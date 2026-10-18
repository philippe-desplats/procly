mod common;

use std::net::TcpStream;
use std::path::Path;

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
