mod common;

use common::{Procly, assert_success, free_port, http_get, stdout, test_server};

#[test]
fn the_test_server_listens_on_port_and_exits_on_request() {
    let procly = Procly::new();
    let port = free_port();

    let output = procly
        .command()
        .args(["run", &test_server().display().to_string()])
        .env("PORT", port.to_string())
        .output()
        .unwrap();
    assert_success(&output);
    let job = stdout(&output).trim().to_owned();

    assert_eq!(http_get(port, "/"), "ok\n");
    assert_eq!(http_get(port, "/exit/7"), "ok\n");
    let output = procly.procly(&["wait", &job]);
    assert_eq!(output.status.code(), Some(7));
    assert_eq!(stdout(&output), format!("listening on {port}\nexiting with 7\n"));
}

#[test]
fn the_test_server_listens_on_a_given_port() {
    let procly = Procly::new();
    let port = free_port();

    let job = procly.run(&[&format!("{} --port {port}", test_server().display())]);

    assert_eq!(http_get(port, "/exit/0"), "ok\n");
    assert_success(&procly.procly(&["wait", &job]));
}
