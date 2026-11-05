//! The app's one way to the Daemon: a [`DaemonClient`] sends a `Request` through `procly-client` and hands back the
//! `Response` as the Daemon wrote it. It only ever connects, it never starts a Daemon (`docs/rules/tauri.md`).

use std::path::PathBuf;

use procly_client::protocol::{Request, Response};
use procly_client::state_dir::StateDir;
use procly_client::{ClientError, connect};
use serde::{Serialize, Serializer};

/// What a Tauri command fails with. It reaches the webview as a rejected promise holding a [`ClientFailure`], or the
/// same shape with the kind `task` when the thread that talks to the Daemon broke.
#[derive(Debug, thiserror::Error)]
pub(crate) enum CommandError {
    #[error(transparent)]
    Client(#[from] ClientError),
    #[error("the thread talking to the Daemon stopped: {0}")]
    Task(#[source] tauri::Error),
}

impl Serialize for CommandError {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Client(error) => error.serialize(serializer),
            Self::Task(_) => TaskFailure {
                kind: "task",
                message: self.to_string(),
            }
            .serialize(serializer),
        }
    }
}

/// The shape of a [`ClientFailure`] for a failure that is not the client's.
#[derive(Serialize)]
struct TaskFailure {
    kind: &'static str,
    message: String,
}

/// The connection point of the app to the Daemon, managed by Tauri and shared by every command.
#[derive(Debug, Clone)]
pub(crate) struct DaemonClient {
    state: StateDir,
}

impl DaemonClient {
    #[must_use]
    pub(crate) fn new(state: StateDir) -> Self {
        Self { state }
    }

    /// The client of the Daemon whose state directory the CLI would use: `$PROCLY_STATE_DIR` first.
    ///
    /// # Errors
    ///
    /// When no state directory can be resolved from the environment.
    pub(crate) fn resolve() -> Result<Self, ClientError> {
        Ok(Self::new(StateDir::resolve()?))
    }

    /// Sends `request` to the Daemon that runs and returns its answer unchanged. The client is blocking, so it runs
    /// on a thread that may block.
    ///
    /// # Errors
    ///
    /// [`ClientError::Unreachable`] when no Daemon runs, which this never changes by starting one.
    pub(crate) async fn send(&self, request: Request) -> Result<Response, CommandError> {
        let state = self.state.clone();
        tauri::async_runtime::spawn_blocking(move || connect(&state)?.send(&request)?.answer(|_| {}))
            .await
            .map_err(CommandError::Task)?
            .map_err(CommandError::from)
    }

    /// Asks for the status of every Worktree the Daemon knows.
    ///
    /// # Errors
    ///
    /// As [`DaemonClient::send`].
    pub(crate) async fn status(&self) -> Result<Response, CommandError> {
        // With `all` the directory only has to exist, the answer does not depend on it.
        self.send(Request::Status {
            cwd: PathBuf::from("/"),
            all: true,
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead as _, BufReader, Write as _};
    use std::os::unix::net::UnixListener;
    use std::thread;

    use procly_client::protocol::{self, RepositoryStatus, StatusReport, Worktree, WorktreeStatus};

    use super::*;

    /// A state directory of its own, short enough for the socket path limit of macOS.
    fn state() -> (tempfile::TempDir, StateDir) {
        let dir = tempfile::Builder::new()
            .prefix("pdk")
            .tempdir()
            .expect("the temporary directory can be created");
        let state = StateDir::at(dir.path());
        (dir, state)
    }

    #[test]
    fn the_answer_of_a_running_daemon_comes_back_unchanged() {
        let (_dir, state) = state();
        let listener = UnixListener::bind(state.socket()).expect("the stand-in Daemon binds the socket");
        let worktree = Worktree {
            path: PathBuf::from("/p/work/acme/web"),
            slug: "web".to_owned(),
            main: true,
        };
        let report = StatusReport {
            repositories: vec![RepositoryStatus {
                git_common_dir: PathBuf::from("/p/work/acme/web/.git"),
                worktrees: vec![WorktreeStatus {
                    worktree: worktree.clone(),
                    branch: Some("main".to_owned()),
                    listed: true,
                    services: Vec::new(),
                    jobs: Vec::new(),
                    foreign: Vec::new(),
                    containers: Vec::new(),
                }],
            }],
            jobs_outside_repositories: Vec::new(),
        };
        let answer = protocol::encode(Response::Status(report)).expect("the answer encodes");
        let daemon = thread::spawn(move || {
            let (stream, _) = listener.accept().expect("the app connects");
            let mut request = String::new();
            BufReader::new(&stream)
                .read_line(&mut request)
                .expect("the request line is read");
            (&stream).write_all(answer.as_bytes()).expect("the answer is written");
            request
        });

        let response =
            tauri::async_runtime::block_on(DaemonClient::new(state).status()).expect("a running Daemon answers");

        let Response::Status(report) = response else {
            panic!("the answer of a Status request is a status");
        };
        assert_eq!(report.repositories[0].worktrees[0].worktree, worktree);
        let request = daemon.join().expect("the stand-in Daemon ends");
        assert!(request.contains(r#""request":"status""#), "{request}");
        assert!(request.contains(r#""all":true"#), "{request}");
    }

    #[test]
    fn without_a_daemon_the_command_fails_as_unreachable_and_starts_nothing() {
        let (dir, state) = state();

        let error = tauri::async_runtime::block_on(DaemonClient::new(state).status()).expect_err("no Daemon runs here");

        let json = serde_json::to_value(&error).expect("the error serializes");
        assert_eq!(json["kind"], "unreachable");
        // Starting a Daemon would leave its socket, lock, pid and log behind.
        let left: Vec<_> = std::fs::read_dir(dir.path())
            .expect("the directory is readable")
            .collect();
        assert!(left.is_empty(), "nothing is created in the state directory: {left:?}");
    }
}
