mod common;

use common::{Procly, assert_success, free_port, http_get, http_response, stdout, test_server};

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
    let (job, _) = common::started(&output);

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

#[test]
fn the_test_server_answers_a_request_for_a_status_with_that_status() {
    let procly = Procly::new();
    let port = free_port();

    let job = procly.run(&[&format!("{} --port {port}", test_server().display())]);

    assert!(
        http_response(port, "/status/418").starts_with("HTTP/1.1 418 "),
        "{}",
        http_response(port, "/status/418")
    );
    assert!(http_response(port, "/").starts_with("HTTP/1.1 200 OK\r\n"));
    assert_eq!(http_get(port, "/exit/0"), "ok\n");
    assert_success(&procly.procly(&["wait", &job]));
}
