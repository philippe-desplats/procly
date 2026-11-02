//! The failure messages of the harness name what held a port a test declared.

mod common;

use std::path::Path;
use std::process::{Child, Command, Stdio};

use common::{Procly, assert_success, free_port, git_repository, http_get, stdout, test_server};

/// A test server started outside any Daemon, listening on a port until the guard is dropped.
struct Holder(Child);

impl Holder {
    fn on(port: u16) -> Self {
        let child = Command::new(test_server())
            .args(["--port", &port.to_string()])
            .stdout(Stdio::null())
            .spawn()
            .expect("start the holder");
        let holder = Self(child);
        assert_eq!(http_get(port, "/"), "ok\n");
        holder
    }

    fn pid(&self) -> u32 {
        self.0.id()
    }
}

impl Drop for Holder {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A `procly.toml` declaring `web` on `port`, which runs the test server on the port of its Lease.
fn web_on(port: u16) -> String {
    format!(
        "[services.web]\nrun = 'exec {}'\nports = {{ http = {port} }}\n",
        test_server().display()
    )
}

fn write(dir: &Path, content: &str) {
    std::fs::write(dir.join("procly.toml"), content).expect("write the config file");
}

#[test]
fn a_lease_that_missed_the_declared_port_names_the_process_that_held_it() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let port = free_port();
    let holder = Holder::on(port);
    write(&main, &web_on(port));
    let output = procly.procly_in(&main, &["up", "web"]);
    assert_success(&output);

    let failure = common::check_declared_port(
        port,
        &stdout(&output),
        &format!("web started http://localhost:{port} port {port}\n"),
    )
    .expect_err("the Lease is not on the declared port");

    assert!(failure.contains(&format!("pid {}", holder.pid())), "{failure}");
    assert!(
        failure.contains(&format!("procly-test-server --port {port}")),
        "{failure}"
    );
}

#[test]
fn a_lease_that_missed_a_port_nobody_holds_any_more_says_so() {
    let port = free_port();

    let failure = common::check_declared_port(port, "web started port 24000\n", "web started port 1\n")
        .expect_err("the reports differ");

    assert!(failure.contains("left: \"web started port 24000\\n\""), "{failure}");
    assert!(
        failure.contains(&format!("port {port}, handed out by free_port()")),
        "{failure}"
    );
    assert!(failure.contains("nothing listens on it any more"), "{failure}");
}

#[test]
fn a_matching_report_passes_without_any_lookup() {
    assert_eq!(common::check_declared_port(1, "same", "same"), Ok(()));
}

#[test]
fn the_diagnostic_names_the_lease_of_the_daemon_that_holds_the_port() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let port = free_port();
    write(&main, &web_on(port));
    assert_success(&procly.procly_in(&main, &["up", "web"]));

    let failure = common::check_declared_port(port, "a", "b").expect_err("the reports differ");

    assert!(
        failure.contains(&format!(
            "the Lease of the Service web in the Worktree {} holds it",
            main.display()
        )),
        "{failure}"
    );
    assert!(failure.contains("procly-test-server"), "{failure}");
}

/// What `procly up` prints on stderr when a Service listens on another port than its Lease.
fn port_mismatch(listens: u16, leased: u16) -> std::process::Output {
    std::process::Output {
        status: std::os::unix::process::ExitStatusExt::from_raw(4 << 8),
        stdout: Vec::new(),
        stderr: format!(
            "web listens on port {listens} instead of its Lease, port {leased} (Port mismatch), its process tree was stopped\n"
        )
        .into_bytes(),
    }
}

#[test]
fn a_port_mismatch_on_a_declared_port_names_the_process_that_holds_it() {
    let port = free_port();
    let holder = Holder::on(port);

    let message = common::failure_message(&port_mismatch(port, 24_000));

    assert!(message.contains("(Port mismatch)"), "{message}");
    assert!(message.contains(&format!("pid {}", holder.pid())), "{message}");
    assert!(
        message.contains(&format!("procly-test-server --port {port}")),
        "{message}"
    );
}

#[test]
fn a_port_mismatch_on_a_port_nothing_listens_on_any_more_says_so() {
    let port = free_port();

    let message = common::failure_message(&port_mismatch(port, 24_000));

    assert!(message.contains("nothing listens on it any more"), "{message}");
}

#[test]
fn a_failure_that_is_not_about_a_port_gets_no_diagnostic() {
    let unrelated = std::process::Output {
        status: std::os::unix::process::ExitStatusExt::from_raw(1 << 8),
        stdout: Vec::new(),
        stderr: b"web failed to start\n".to_vec(),
    };
    let not_handed_out = common::failure_message(&port_mismatch(1, 2));

    assert!(!common::failure_message(&unrelated).contains("handed out"));
    assert!(!not_handed_out.contains("handed out"), "{not_handed_out}");
}

fn lookup(candidate: &[&str], timeout_ms: u64) -> common::port_holders::Lookup {
    common::port_holders::Lookup {
        candidates: vec![candidate.iter().map(|part| (*part).to_owned()).collect()],
        timeout: std::time::Duration::from_millis(timeout_ms),
    }
}

#[test]
fn a_missing_lookup_tool_is_reported_instead_of_failing() {
    let message = common::port_holders::diagnose_with(12_345, &lookup(&["/nonexistent/lsof"], 1000));

    assert!(
        message.contains("the holders could not be listed: /nonexistent/lsof was not found"),
        "{message}"
    );
}

#[test]
fn a_stuck_lookup_tool_is_reported_after_its_time_and_does_not_hang() {
    let start = std::time::Instant::now();

    // The arguments of the lookup become the positional parameters of the shell, which ignores them.
    let message = common::port_holders::diagnose_with(12_345, &lookup(&["sh", "-c", "exec sleep 30", "sh"], 300));

    assert!(
        message.contains("the holders could not be listed: sh did not answer within 300ms"),
        "{message}"
    );
    assert!(
        start.elapsed() < std::time::Duration::from_secs(10),
        "the lookup was not bounded"
    );
}
