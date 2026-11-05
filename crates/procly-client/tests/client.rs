//! The client through its public interface, against a stand-in Daemon bound on the socket of a temporary state
//! directory: framing, streams of responses, `connect` and `connect_or_start`.

use std::fs;
use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use procly_client::protocol::{self, DecodeError, Request, Response};
use procly_client::state_dir::StateDir;
use procly_client::{ClientError, connect, connect_or_start};
use tempfile::TempDir;

/// How long a test waits for something a process it started does. A freshly written script can take long on its
/// first exec, so this is far beyond what a fast machine needs.
const DEADLINE: Duration = Duration::from_secs(60);

/// A state directory of its own, short enough for the socket path limit of macOS.
fn state() -> (TempDir, StateDir) {
    let dir = tempfile::Builder::new()
        .prefix("pcc")
        .tempdir()
        .expect("the temporary directory can be created");
    let state = StateDir::at(dir.path());
    (dir, state)
}

/// A stand-in Daemon: accepts one connection, reads its one request line, writes `lines` and closes. Returns the
/// request line it read.
fn daemon(state: &StateDir, lines: Vec<String>) -> JoinHandle<String> {
    let listener = UnixListener::bind(state.socket()).expect("the stand-in Daemon binds the socket");
    thread::spawn(move || answer(&listener, &lines))
}

fn answer(listener: &UnixListener, lines: &[String]) -> String {
    let (stream, _) = listener.accept().expect("the client connects");
    let mut request = String::new();
    BufReader::new(&stream)
        .read_line(&mut request)
        .expect("the client sends its request");
    for line in lines {
        (&stream)
            .write_all(line.as_bytes())
            .expect("the client reads the responses");
    }
    request
}

fn line(response: Response) -> String {
    protocol::encode(response).expect("a response without a path always encodes")
}

fn progress(command: &str) -> Response {
    Response::SetupProgress {
        command: command.to_owned(),
        elapsed_ms: 10,
        log: PathBuf::from("/tmp/setup.log"),
    }
}

fn logs(output: &str) -> Response {
    Response::Logs {
        output: output.to_owned(),
    }
}

fn wait() -> Request {
    Request::Wait {
        job: "3f9a1c2e".to_owned(),
    }
}

fn eventually(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + DEADLINE;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(20));
    }
}

/// An executable that records the arguments it was started with in `marker`, and does nothing else.
fn recording_exe(dir: &Path, marker: &Path) -> PathBuf {
    let exe = dir.join("fake-procly");
    fs::write(&exe, format!("#!/bin/sh\necho \"$@\" > '{}'\n", marker.display()))
        .expect("the executable can be written");
    fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).expect("the executable can be made executable");
    exe
}

#[test]
fn the_request_is_one_line_and_every_response_line_is_read_in_order_until_the_daemon_closes() {
    let (_dir, state) = state();
    let daemon = daemon(
        &state,
        vec![line(progress("bun install")), line(logs("a\n")), line(logs("b\n"))],
    );

    let responses: Vec<Response> = connect(&state)
        .unwrap()
        .send(&wait())
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();

    assert_eq!(
        daemon.join().unwrap(),
        "{\"version\":22,\"request\":\"wait\",\"job\":\"3f9a1c2e\"}\n"
    );
    assert!(matches!(&responses[..], [
        Response::SetupProgress { command, .. },
        Response::Logs { output: a },
        Response::Logs { output: b },
    ] if command == "bun install" && a == "a\n" && b == "b\n"));
}

#[test]
fn the_answer_is_the_first_response_that_is_not_setup_progress() {
    let (_dir, state) = state();
    let daemon = daemon(
        &state,
        vec![
            line(progress("bun install")),
            line(progress("make build")),
            line(logs("done\n")),
            line(logs("never read\n")),
        ],
    );

    let mut seen = Vec::new();
    let answer = connect(&state)
        .unwrap()
        .send(&wait())
        .unwrap()
        .answer(|response| {
            if let Response::SetupProgress { command, .. } = response {
                seen.push(command.clone());
            }
        })
        .unwrap();

    daemon.join().unwrap();
    assert_eq!(seen, ["bun install", "make build"]);
    assert!(matches!(answer, Response::Logs { output } if output == "done\n"));
}

#[test]
fn a_daemon_that_closes_before_answering_is_no_response() {
    let (_dir, state) = state();
    let daemon = daemon(&state, vec![line(progress("bun install"))]);

    let result = connect(&state).unwrap().send(&wait()).unwrap().answer(|_| ());

    daemon.join().unwrap();
    assert!(matches!(result, Err(ClientError::NoResponse)), "{result:?}");
}

#[test]
fn a_response_of_another_protocol_version_ends_the_stream_with_its_version() {
    let (_dir, state) = state();
    let daemon = daemon(
        &state,
        vec![
            "{\"version\":99,\"response\":\"teleported\"}\n".to_owned(),
            line(logs("never read\n")),
        ],
    );

    let mut responses = connect(&state).unwrap().send(&wait()).unwrap();

    assert!(matches!(
        responses.next(),
        Some(Err(ClientError::Decode(DecodeError::Version(99))))
    ));
    assert!(responses.next().is_none());
    daemon.join().unwrap();
}

#[test]
fn connect_without_a_daemon_fails_and_starts_nothing() {
    let (_dir, state) = state();

    let result = connect(&state);

    assert!(matches!(result, Err(ClientError::Unreachable { ref path, .. }) if *path == state.socket()));
    assert!(!state.log().exists(), "connect must not prepare a Daemon start");
}

#[test]
fn connect_or_start_starts_the_executable_it_is_given_with_the_daemon_argument() {
    let (dir, state) = state();
    let marker = dir.path().join("started");
    let exe = recording_exe(dir.path(), &marker);
    // The stand-in binds the socket only once the call opened the log of the Daemon, which it does after finding no
    // Daemon and before starting the executable.
    let (socket, log) = (state.socket(), state.log());
    let daemon = thread::spawn(move || {
        eventually("the call to prepare a Daemon start", || log.exists());
        let listener = UnixListener::bind(socket).unwrap();
        answer(&listener, &[line(logs("ok\n"))])
    });

    let answer = connect_or_start(&state, &exe)
        .unwrap()
        .send(&wait())
        .unwrap()
        .answer(|_| ())
        .unwrap();

    daemon.join().unwrap();
    assert!(matches!(answer, Response::Logs { output } if output == "ok\n"));
    eventually("the executable to record its arguments", || {
        fs::read_to_string(&marker).is_ok_and(|args| args == "daemon\n")
    });
    assert!(
        state.log().exists(),
        "the Daemon writes its output to the log of the state directory"
    );
}

#[test]
fn connect_or_start_with_a_daemon_running_starts_nothing() {
    let (dir, state) = state();
    let marker = dir.path().join("started");
    let exe = recording_exe(dir.path(), &marker);
    let daemon = daemon(&state, vec![line(logs("ok\n"))]);

    let answer = connect_or_start(&state, &exe)
        .unwrap()
        .send(&wait())
        .unwrap()
        .answer(|_| ())
        .unwrap();

    daemon.join().unwrap();
    assert!(matches!(answer, Response::Logs { .. }));
    assert!(!marker.exists(), "a running Daemon is never started again");
}

#[test]
fn connect_or_start_names_an_executable_that_cannot_start() {
    let (dir, state) = state();
    let exe = dir.path().join("missing-procly");

    let result = connect_or_start(&state, &exe);

    assert!(
        matches!(result, Err(ClientError::StartDaemon { exe: ref named, .. }) if *named == exe),
        "{result:?}"
    );
}

#[test]
fn an_error_serializes_as_its_kind_and_its_message() {
    let (_dir, state) = state();
    let error = connect(&state).expect_err("no Daemon runs here");
    assert!(matches!(error, ClientError::Unreachable { .. }));
    let json = serde_json::to_value(&error).expect("an error serializes");
    assert_eq!(json["kind"], "unreachable");
    assert_eq!(json["message"], error.to_string());

    let json = serde_json::to_value(ClientError::NoResponse).expect("an error serializes");
    assert_eq!(json["kind"], "no_response");
}
