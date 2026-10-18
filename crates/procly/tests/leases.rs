mod common;

use std::path::Path;

use common::{Procly, assert_success, free_port, git_repository, stdout, test_server};

/// Writes `content` to `name` at the root of the Worktree `dir`.
fn write(dir: &Path, name: &str, content: &str) {
    std::fs::write(dir.join(name), content).expect("write the config file");
}

/// A `procly.toml` declaring `web`, which runs the test server on its leased `http` port, declared as `port`.
fn web_declaring(port: u16) -> String {
    format!(
        "[services.web]\nrun = '{} --port {{ports.http}}'\nports = {{ http = {port} }}\n",
        test_server().display()
    )
}

/// The port `procly up` printed at the end of its line.
fn port_of(output: &std::process::Output) -> u16 {
    assert_success(output);
    let line = stdout(output);
    line.trim_end()
        .rsplit(' ')
        .next()
        .and_then(|port| port.parse().ok())
        .unwrap_or_else(|| panic!("no port at the end of {line:?}"))
}

#[test]
fn the_main_worktree_gets_the_declared_port_through_the_command_placeholder() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let port = free_port();
    write(&main, "procly.toml", &web_declaring(port));

    let output = procly.procly_in(&main, &["up", "web"]);

    assert_success(&output);
    assert_eq!(
        stdout(&output),
        format!("web started http://localhost:{port} port {port}\n")
    );
    assert_eq!(common::http_get(port, "/"), "ok\n");
}

#[test]
fn a_second_worktree_gets_a_port_from_the_range_and_not_the_declared_one() {
    let procly = Procly::new();
    let (start, end) = procly.narrow_range();
    let main = git_repository(procly.scratch(), "app");
    let other = common::git_worktree_add(&main, "app-other");
    let declared = free_port();
    write(&main, "procly.toml", &web_declaring(declared));
    write(&other, "procly.toml", &web_declaring(declared));

    let in_main = port_of(&procly.procly_in(&main, &["up", "web"]));
    let in_other = port_of(&procly.procly_in(&other, &["up", "web"]));

    assert_eq!(in_main, declared);
    assert!((start..=end).contains(&in_other), "{in_other} is outside {start}-{end}");
    assert_eq!(common::http_get(in_other, "/"), "ok\n");
}

#[test]
fn a_port_held_by_a_foreign_process_is_skipped() {
    let procly = Procly::new();
    let (start, _) = procly.narrow_range();
    let main = git_repository(procly.scratch(), "app");
    let other = common::git_worktree_add(&main, "app-other");
    write(&other, "procly.toml", &web_declaring(free_port()));
    let foreign = std::net::TcpListener::bind(("127.0.0.1", start)).expect("hold the first port of the Range");

    let port = port_of(&procly.procly_in(&other, &["up", "web"]));

    assert_ne!(port, start);
    drop(foreign);
}

#[test]
fn leases_stay_the_same_across_restarts_of_the_service_and_of_the_daemon() {
    let procly = Procly::new();
    procly.narrow_range();
    let main = git_repository(procly.scratch(), "app");
    let (first, second) = (
        common::git_worktree_add(&main, "app-first"),
        common::git_worktree_add(&main, "app-second"),
    );
    for worktree in [&first, &second] {
        write(worktree, "procly.toml", &web_declaring(free_port()));
    }
    let in_first = port_of(&procly.procly_in(&first, &["up", "web"]));
    let in_second = port_of(&procly.procly_in(&second, &["up", "web"]));

    assert_success(&procly.procly_in(&second, &["down", "web"]));
    assert_eq!(port_of(&procly.procly_in(&second, &["up", "web"])), in_second);

    procly.stop_daemon();
    // The second Worktree asks first, a Daemon that forgot would hand it the port of the first one.
    assert_eq!(port_of(&procly.procly_in(&second, &["up", "web"])), in_second);
    assert_eq!(port_of(&procly.procly_in(&first, &["up", "web"])), in_first);
}

#[test]
fn concurrent_ups_from_several_worktrees_never_get_the_same_port() {
    let procly = Procly::new();
    procly.narrow_range();
    let main = git_repository(procly.scratch(), "app");
    let worktrees: Vec<_> = (0..4)
        .map(|i| common::git_worktree_add(&main, &format!("app-{i}")))
        .collect();
    for worktree in &worktrees {
        write(worktree, "procly.toml", &web_declaring(free_port()));
    }

    let ups: Vec<_> = worktrees
        .iter()
        .map(|worktree| {
            procly
                .command()
                .current_dir(worktree)
                .args(["up", "web"])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .expect("procly up starts")
        })
        .collect();
    let mut ports: Vec<u16> = ups
        .into_iter()
        .map(|up| port_of(&up.wait_with_output().expect("procly up ends")))
        .collect();

    ports.sort_unstable();
    ports.dedup();
    assert_eq!(ports.len(), 4, "{ports:?}");
}

#[test]
fn an_unreadable_lease_document_fails_up_with_125_naming_it() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    write(&main, "procly.toml", &web_declaring(free_port()));
    std::fs::write(procly.state_dir().join("leases.json"), "{\"version\": 1, \"leases\": [").unwrap();

    let output = procly.procly_in(&main, &["up", "web"]);

    assert_eq!(output.status.code(), Some(125));
    assert!(common::stderr(&output).contains("leases.json"), "{}", common::stderr(&output));
}
