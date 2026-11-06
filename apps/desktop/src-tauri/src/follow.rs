//! Following the output of a Service or a Job: one `Logs { follow: true }` at a time, carried to the window by a
//! `tauri::ipc::Channel` that forwards every response unchanged (`docs/rules/tauri.md`).
//!
//! Cancelling a follow means closing its connection, which is how the Daemon learns that nobody reads any more. The
//! connection is closed when the window picks another item (a newer follow, or a stop), and when the window is hidden,
//! minimized or closed, which Rust learns from the window itself because a webview that is gone runs no cleanup.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use procly_client::protocol::{Request, Response};
use procly_client::state_dir::StateDir;
use procly_client::{ClientError, Closer, connect};
use tauri::State;
use tauri::ipc::Channel;

use crate::daemon::{CommandError, DaemonClient};
use crate::poller::WindowState;

/// The event that tells the window it is visible again (`true`) or no longer is (`false`), so that a follow cancelled
/// by hiding the window starts again, from a fresh backlog, once it comes back.
pub(crate) const WINDOW_VISIBLE: &str = "window-visible";

/// What the app knows of its follow, managed by Tauri. Cloning shares it.
#[derive(Debug, Clone)]
pub(crate) struct Follower(Arc<Mutex<Follow>>);

#[derive(Debug)]
struct Follow {
    /// The ticket of the newest command the window sent, a follow or a stop. The window numbers them in order, so a
    /// command that arrives after a newer one is stale and does nothing.
    latest: u64,
    /// Tickets up to this one were cancelled by the window being hidden, even when their connection was not open yet.
    hidden_up_to: u64,
    /// The connection of the follow that runs.
    active: Option<Closer>,
    visible: bool,
}

impl Follower {
    pub(crate) fn new() -> Self {
        Self(Arc::new(Mutex::new(Follow {
            latest: 0,
            hidden_up_to: 0,
            active: None,
            visible: true,
        })))
    }

    fn lock(&self) -> MutexGuard<'_, Follow> {
        // Nothing here panics while holding the lock, and a poisoned state is still a consistent one.
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Makes `ticket` the follow that runs: the one before it is closed. A stale ticket gets `false` and follows
    /// nothing.
    pub(crate) fn begin(&self, ticket: u64) -> bool {
        let mut follow = self.lock();
        if ticket <= follow.latest {
            return false;
        }
        follow.latest = ticket;
        if let Some(previous) = follow.active.take() {
            previous.close();
        }
        true
    }

    /// Hands over the connection of the follow `ticket`. It is closed on the spot when the follow was cancelled in
    /// the meantime, and `false` says so.
    fn attach(&self, ticket: u64, closer: Closer) -> bool {
        let mut follow = self.lock();
        if follow.latest != ticket || ticket <= follow.hidden_up_to {
            closer.close();
            return false;
        }
        follow.active = Some(closer);
        true
    }

    /// The follow `ticket` is over, whichever way it ended.
    fn finish(&self, ticket: u64) {
        let mut follow = self.lock();
        if follow.latest == ticket {
            follow.active = None;
        }
    }

    /// Cancels the follow that runs, when the window no longer shows its item. A stale ticket does nothing.
    pub(crate) fn stop(&self, ticket: u64) {
        let mut follow = self.lock();
        if ticket <= follow.latest {
            return;
        }
        follow.latest = ticket;
        if let Some(active) = follow.active.take() {
            active.close();
        }
    }

    /// Tells the follow where the window stands. Hidden, minimized or closed, it cancels the follow. Returns the
    /// visibility when it changed, `None` when it did not.
    pub(crate) fn window(&self, state: WindowState) -> Option<bool> {
        let visible = state != WindowState::Closed;
        let mut follow = self.lock();
        if follow.visible == visible {
            return None;
        }
        follow.visible = visible;
        if !visible {
            follow.hidden_up_to = follow.latest;
            if let Some(active) = follow.active.take() {
                active.close();
            }
        }
        Some(visible)
    }
}

/// Sends `request` and hands every response to `sink` as it comes, unchanged, until the stream ends, `sink` refuses
/// one (the window is gone) or the follow is cancelled. A cancelled follow ends quietly, it is not an error.
///
/// # Errors
///
/// When the Daemon cannot be reached, answers in another protocol version, or breaks the stream.
pub(crate) fn stream(
    state: &StateDir,
    follower: &Follower,
    ticket: u64,
    request: &Request,
    mut sink: impl FnMut(Response) -> bool,
) -> Result<(), CommandError> {
    let outcome = forward(state, follower, ticket, request, &mut sink);
    follower.finish(ticket);
    outcome
}

fn forward(
    state: &StateDir,
    follower: &Follower,
    ticket: u64,
    request: &Request,
    sink: &mut impl FnMut(Response) -> bool,
) -> Result<(), CommandError> {
    let responses = connect(state)?.send(request)?;
    let closer = responses.closer().map_err(|source| ClientError::Connection {
        path: state.socket(),
        source,
    })?;
    if !follower.attach(ticket, closer) {
        return Ok(());
    }
    for response in responses {
        let response = response?;
        let last = matches!(response, Response::LogsEnded | Response::Error { .. });
        if !sink(response) {
            // Nobody reads any more. Returning lets go of the connection, and the Daemon learns it from there.
            return Ok(());
        }
        if last {
            break;
        }
    }
    Ok(())
}

/// Follows the output of the Job or the Service `name`: its backlog, then each line as it comes, through `on_output`.
/// The command answers when the follow is over. The window numbers its commands with `ticket`, so one that arrives
/// late is ignored.
///
/// # Errors
///
/// See [`stream`].
#[tauri::command]
pub(crate) async fn follow_logs(
    ticket: u64,
    name: String,
    cwd: PathBuf,
    on_output: Channel<Response>,
    client: State<'_, DaemonClient>,
    follower: State<'_, Follower>,
) -> Result<(), CommandError> {
    let follower = follower.inner().clone();
    if !follower.begin(ticket) {
        return Ok(());
    }
    let state = client.state().clone();
    let request = Request::Logs {
        name,
        cwd,
        // A Service or a Job needs no environment of the caller.
        env: Vec::new(),
        follow: true,
    };
    tauri::async_runtime::spawn_blocking(move || {
        stream(&state, &follower, ticket, &request, |response| {
            on_output.send(response).is_ok()
        })
    })
    .await
    .map_err(CommandError::Task)?
}

/// Cancels the follow that runs. The window calls it when the item shown in the inspector changes.
#[tauri::command]
#[expect(
    clippy::needless_pass_by_value,
    reason = "Tauri hands managed state to a command as a `State`"
)]
pub(crate) fn stop_follow(ticket: u64, follower: State<'_, Follower>) {
    follower.stop(ticket);
}

/// The recent output of `name`, once, as `Logs` answers it without `follow`. It is how a Compose service is shown,
/// since it cannot be followed. The environment is the one of the app: the Daemon uses its held environment of the
/// Service when it has one, and this one when it holds none, in which case `docker compose` may fail and the window
/// shows why. The `PATH` is never completed with guessed directories.
///
/// # Errors
///
/// As [`DaemonClient::send`].
#[tauri::command]
pub(crate) async fn service_logs(
    name: String,
    cwd: PathBuf,
    client: State<'_, DaemonClient>,
) -> Result<Response, CommandError> {
    client
        .send(Request::Logs {
            name,
            cwd,
            env: std::env::vars().collect(),
            follow: false,
        })
        .await
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead as _, BufReader, Read as _, Write as _};
    use std::os::unix::net::UnixListener;
    use std::sync::mpsc;
    use std::thread::{self, JoinHandle};
    use std::time::Duration;

    use procly_client::protocol;
    use tauri::test::{mock_builder, mock_context, noop_assets};
    use tauri::{Listener as _, Manager as _, WebviewUrl, WebviewWindowBuilder, WindowEvent};

    use super::*;

    fn state() -> (tempfile::TempDir, StateDir) {
        let dir = tempfile::Builder::new()
            .prefix("pdf")
            .tempdir()
            .expect("the temporary directory can be created");
        let state = StateDir::at(dir.path());
        (dir, state)
    }

    fn follow_request() -> Request {
        Request::Logs {
            name: "api".to_owned(),
            cwd: PathBuf::from("/p/work/acme/web"),
            env: Vec::new(),
            follow: true,
        }
    }

    fn line(response: Response) -> String {
        protocol::encode(response).expect("a response encodes")
    }

    /// A stand-in Daemon that reads the request, writes `lines`, then keeps the connection open until the client
    /// closes it. Returns the request it read.
    fn daemon_that_waits(state: &StateDir, lines: Vec<String>) -> JoinHandle<String> {
        let listener = UnixListener::bind(state.socket()).expect("the stand-in Daemon binds the socket");
        thread::spawn(move || {
            let (stream, _) = listener.accept().expect("the app connects");
            let mut request = String::new();
            BufReader::new(&stream)
                .read_line(&mut request)
                .expect("the request line is read");
            for line in lines {
                (&stream).write_all(line.as_bytes()).expect("the response is written");
            }
            let mut rest = Vec::new();
            (&stream).read_to_end(&mut rest).expect("the app closes the connection");
            request
        })
    }

    /// Runs a follow on its own thread, returns a receiver of what it forwarded, and the thread.
    fn run(
        state: StateDir,
        follower: &Follower,
        ticket: u64,
    ) -> (mpsc::Receiver<Response>, JoinHandle<Result<(), CommandError>>) {
        let (sent, received) = mpsc::channel();
        let follower = follower.clone();
        let thread = thread::spawn(move || {
            stream(&state, &follower, ticket, &follow_request(), |response| {
                sent.send(response).is_ok()
            })
        });
        (received, thread)
    }

    fn next(received: &mpsc::Receiver<Response>) -> Response {
        received
            .recv_timeout(Duration::from_secs(30))
            .expect("the follow forwards a response")
    }

    #[test]
    fn the_backlog_each_output_and_the_end_are_forwarded_unchanged_in_order() {
        let (_dir, state) = state();
        let listener = UnixListener::bind(state.socket()).expect("the stand-in Daemon binds the socket");
        let daemon = thread::spawn(move || {
            let (stream, _) = listener.accept().expect("the app connects");
            let mut request = String::new();
            BufReader::new(&stream)
                .read_line(&mut request)
                .expect("the request line is read");
            for response in [
                Response::Logs {
                    output: "booting\n".to_owned(),
                },
                Response::Output {
                    text: "ready\n".to_owned(),
                },
                Response::OutputDropped { bytes: 4096 },
                Response::LogsEnded,
            ] {
                (&stream).write_all(line(response).as_bytes()).expect("written");
            }
            request
        });
        let follower = Follower::new();
        assert!(follower.begin(1));

        let (received, thread) = run(state, &follower, 1);

        assert!(matches!(next(&received), Response::Logs { output } if output == "booting\n"));
        assert!(matches!(next(&received), Response::Output { text } if text == "ready\n"));
        assert!(matches!(next(&received), Response::OutputDropped { bytes: 4096 }));
        assert!(matches!(next(&received), Response::LogsEnded));
        thread.join().expect("the follow ends").expect("it ends well");
        let request = daemon.join().expect("the stand-in Daemon ends");
        assert!(request.contains(r#""request":"logs""#), "{request}");
        assert!(request.contains(r#""name":"api""#), "{request}");
        assert!(request.contains(r#""follow":true"#), "{request}");
    }

    #[test]
    fn an_error_of_the_daemon_is_forwarded_and_ends_the_follow() {
        let (_dir, state) = state();
        let daemon = daemon_that_waits(
            &state,
            vec![line(Response::Error {
                message: "no job or service api".to_owned(),
            })],
        );
        let follower = Follower::new();
        assert!(follower.begin(1));

        let (received, thread) = run(state, &follower, 1);

        assert!(matches!(next(&received), Response::Error { message } if message == "no job or service api"));
        thread.join().expect("the follow ends").expect("it ends well");
        // The follow let go of the connection, so the stand-in reads the end of it.
        daemon.join().expect("the stand-in Daemon sees the connection close");
    }

    /// The follow in `received`/`thread` forwarded its backlog and now waits on a silent Daemon.
    fn waiting_follow(
        state: StateDir,
        follower: &Follower,
        ticket: u64,
    ) -> (JoinHandle<String>, JoinHandle<Result<(), CommandError>>) {
        let daemon = daemon_that_waits(
            &state,
            vec![line(Response::Logs {
                output: "backlog\n".to_owned(),
            })],
        );
        assert!(follower.begin(ticket));
        let (received, thread) = run(state, follower, ticket);
        assert!(matches!(next(&received), Response::Logs { .. }));
        (daemon, thread)
    }

    #[test]
    fn a_newer_follow_closes_the_connection_of_the_one_before() {
        let (_dir, state) = state();
        let follower = Follower::new();
        let (daemon, thread) = waiting_follow(state, &follower, 1);

        assert!(follower.begin(2));

        thread.join().expect("the first follow ends").expect("quietly");
        daemon.join().expect("the Daemon of the first follow sees it close");
    }

    #[test]
    fn stopping_closes_the_connection_of_the_follow_that_runs() {
        let (_dir, state) = state();
        let follower = Follower::new();
        let (daemon, thread) = waiting_follow(state, &follower, 1);

        follower.stop(2);

        thread.join().expect("the follow ends").expect("quietly");
        daemon.join().expect("the Daemon sees the connection close");
    }

    #[test]
    fn hiding_the_window_closes_the_connection_and_coming_back_says_so() {
        let (_dir, state) = state();
        let follower = Follower::new();
        let (daemon, thread) = waiting_follow(state, &follower, 1);

        assert_eq!(follower.window(WindowState::Closed), Some(false));

        thread.join().expect("the follow ends").expect("quietly");
        daemon.join().expect("the Daemon sees the connection close");
        assert_eq!(follower.window(WindowState::Closed), None, "no change, no event");
        assert_eq!(follower.window(WindowState::Visible), Some(true));
        assert_eq!(follower.window(WindowState::Front), None);
    }

    #[test]
    fn the_window_event_handler_closes_the_follow_and_tells_the_window_when_it_hides_and_returns() {
        let (_dir, state) = state();
        let follower = Follower::new();
        let (daemon, thread) = waiting_follow(state, &follower, 1);
        // The handler of the app, run on the events of a window of the mock runtime.
        let app = mock_builder()
            .build(mock_context(noop_assets()))
            .expect("the mock app builds");
        app.manage(follower);
        let window = WebviewWindowBuilder::new(&app, "main", WebviewUrl::default())
            .build()
            .expect("the mock window opens");
        let window = window.as_ref().window();
        let (told, heard) = mpsc::channel();
        app.listen(WINDOW_VISIBLE, move |event| {
            told.send(event.payload().to_owned())
                .expect("the test still listens");
        });

        // A window that is gone is no longer looked at, which is how the app learns that it is hidden.
        crate::follow_window(&window, &WindowEvent::Destroyed);

        // Checked first, so that a handler that tells the follower the wrong state fails here instead of waiting on
        // a connection that stays open.
        assert_eq!(heard.recv_timeout(Duration::from_secs(5)).as_deref(), Ok("false"));
        thread.join().expect("the follow ends").expect("quietly");
        daemon.join().expect("the Daemon sees the connection close");
        // The mock window is shown and not in front: giving it focus again is the window coming back.
        crate::follow_window(&window, &WindowEvent::Focused(true));
        assert_eq!(heard.recv_timeout(Duration::from_secs(5)).as_deref(), Ok("true"));
    }

    #[test]
    fn a_snapshot_asks_for_the_logs_once_with_the_environment_of_the_app() {
        let (_dir, state) = state();
        let listener = UnixListener::bind(state.socket()).expect("the stand-in Daemon binds the socket");
        let daemon = thread::spawn(move || {
            let (stream, _) = listener.accept().expect("the app connects");
            let mut request = String::new();
            BufReader::new(&stream)
                .read_line(&mut request)
                .expect("the request line is read");
            let answer = line(Response::Logs {
                output: "db ready\n".to_owned(),
            });
            (&stream).write_all(answer.as_bytes()).expect("written");
            request
        });
        let app = mock_builder()
            .build(mock_context(noop_assets()))
            .expect("the mock app builds");
        app.manage(DaemonClient::new(state, PathBuf::from("/p/bin/procly")));

        let response = tauri::async_runtime::block_on(service_logs(
            "db".to_owned(),
            PathBuf::from("/p/work/acme/web"),
            app.state::<DaemonClient>(),
        ))
        .expect("a running Daemon answers");

        assert!(matches!(response, Response::Logs { output } if output == "db ready\n"));
        let request = daemon.join().expect("the stand-in Daemon ends");
        let Ok(Request::Logs { name, cwd, env, follow }) = protocol::decode::<Request>(request.trim_end()) else {
            panic!("the app asked for the logs: {request}");
        };
        assert_eq!(name, "db");
        assert_eq!(cwd, PathBuf::from("/p/work/acme/web"));
        assert!(!follow, "a Compose service is read once, it is never followed");
        assert!(!env.is_empty(), "the test process has an environment");
        assert_eq!(env, std::env::vars().collect::<Vec<_>>());
    }

    #[test]
    fn a_follow_cancelled_before_its_connection_was_open_follows_nothing() {
        let (_dir, state) = state();
        let daemon = daemon_that_waits(&state, Vec::new());
        let follower = Follower::new();
        assert!(follower.begin(1));
        // The window is hidden between the command and the connection.
        follower.window(WindowState::Closed);

        stream(&state, &follower, 1, &follow_request(), |_| true).expect("a cancelled follow ends quietly");

        daemon.join().expect("the Daemon sees the connection close at once");
    }

    #[test]
    fn a_command_that_arrives_after_a_newer_one_does_nothing() {
        let follower = Follower::new();
        assert!(follower.begin(5));

        assert!(!follower.begin(4), "an older follow is stale");
        assert!(!follower.begin(5), "so is a repeated one");
        follower.stop(3);
        assert!(follower.begin(6), "a stale stop changed nothing");
    }

    #[test]
    fn a_window_that_stops_reading_closes_the_follow() {
        let (_dir, state) = state();
        let daemon = daemon_that_waits(
            &state,
            vec![line(Response::Logs {
                output: "backlog\n".to_owned(),
            })],
        );
        let follower = Follower::new();
        assert!(follower.begin(1));

        // The sink refuses the first response, as a Channel to a window that is gone does.
        let outcome = stream(&state, &follower, 1, &follow_request(), |_| false);

        outcome.expect("a window that is gone is not a failure");
        daemon.join().expect("the Daemon sees the connection close");
    }

    #[test]
    fn without_a_daemon_the_follow_fails_as_unreachable() {
        let (_dir, state) = state();
        let follower = Follower::new();
        assert!(follower.begin(1));

        let error = stream(&state, &follower, 1, &follow_request(), |_| true).expect_err("no Daemon runs here");

        let json = serde_json::to_value(&error).expect("the error serializes");
        assert_eq!(json["kind"], "unreachable");
    }
}
