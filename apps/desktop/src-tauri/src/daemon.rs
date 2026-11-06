//! The app's one way to the Daemon: a [`DaemonClient`] sends a `Request` through `procly-client` and hands back the
//! `Response` as the Daemon wrote it. It only ever connects, it never starts a Daemon (`docs/rules/tauri.md`).

use std::io;
use std::path::{Path, PathBuf};

use procly_client::protocol::{DecodeError, PROTOCOL_VERSION, Request, Response};
use procly_client::state_dir::{StateDir, StateDirError};
use procly_client::{ClientError, connect};
use serde::{Serialize, Serializer};

use crate::poller::Source;

/// The name of the embedded `procly`, which sits beside the executable of the app.
const SIDECAR: &str = "procly";

/// Where the embedded `procly` is, given the executable of the app: beside it, `Procly.app/Contents/MacOS/procly` in
/// a bundle, `target/<profile>/procly` when the app runs from the workspace. Never looked up on the `PATH`.
pub(crate) fn sidecar_beside(app: &Path) -> PathBuf {
    app.with_file_name(SIDECAR)
}

/// What the app cannot do before its window opens.
#[derive(Debug, thiserror::Error)]
pub(crate) enum SetupError {
    #[error(transparent)]
    State(#[from] StateDirError),
    #[error("cannot locate the executable of the app")]
    Executable(#[source] io::Error),
}

/// What a Tauri command fails with. It reaches the webview as a rejected promise holding a [`ClientFailure`], or the
/// same shape with another kind: `task` when the thread that talks to the Daemon broke, `sidecar` when the embedded
/// `procly` cannot run, `daemon_stop` when `procly daemon stop` failed, `doctor` when `procly doctor` gave no document,
/// and `protocol_mismatch` when the Daemon speaks another protocol version.
#[derive(Debug, thiserror::Error)]
pub(crate) enum CommandError {
    #[error(transparent)]
    Client(#[from] ClientError),
    #[error("the thread talking to the Daemon stopped: {0}")]
    Task(#[source] tauri::Error),
    #[error("cannot run the embedded procly at {path}")]
    Sidecar {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// `procly daemon stop` failed, `message` is what it wrote on its standard error.
    #[error("{message}")]
    Stop { message: String },
    /// `procly doctor` could not give its document, `message` says why.
    #[error("{message}")]
    Doctor { message: String },
}

impl CommandError {
    /// Whether no Daemon accepted the connection, as opposed to one that answered badly.
    pub(crate) fn is_unreachable(&self) -> bool {
        matches!(self, Self::Client(ClientError::Unreachable { .. }))
    }
}

impl Serialize for CommandError {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Client(ClientError::Decode(DecodeError::Version(daemon))) => ProtocolMismatch {
                kind: "protocol_mismatch",
                message: self.to_string(),
                daemon_protocol: *daemon,
                app_protocol: PROTOCOL_VERSION,
            }
            .serialize(serializer),
            Self::Client(error) => error.serialize(serializer),
            Self::Task(_) => OtherFailure {
                kind: "task",
                message: self.to_string(),
            }
            .serialize(serializer),
            Self::Sidecar { .. } => OtherFailure {
                kind: "sidecar",
                message: self.to_string(),
            }
            .serialize(serializer),
            Self::Stop { .. } => OtherFailure {
                kind: "daemon_stop",
                message: self.to_string(),
            }
            .serialize(serializer),
            Self::Doctor { .. } => OtherFailure {
                kind: "doctor",
                message: self.to_string(),
            }
            .serialize(serializer),
        }
    }
}

/// The shape of a [`ClientFailure`] for a failure that is not the client's.
#[derive(Serialize)]
struct OtherFailure {
    kind: &'static str,
    message: String,
}

/// A Daemon that answers in another protocol version. The window compares the 2 numbers to tell an older Daemon, which
/// can be restarted, from a newer one, which asks for an update of the app.
#[derive(Serialize)]
struct ProtocolMismatch {
    kind: &'static str,
    message: String,
    daemon_protocol: u32,
    app_protocol: u32,
}

/// The connection point of the app to the Daemon, managed by Tauri and shared by every command.
#[derive(Debug, Clone)]
pub(crate) struct DaemonClient {
    state: StateDir,
    /// The `procly` the app embeds, which it passes to every CLI command and to `connect_or_start`.
    procly: PathBuf,
}

impl DaemonClient {
    #[must_use]
    pub(crate) fn new(state: StateDir, procly: PathBuf) -> Self {
        Self { state, procly }
    }

    /// The client of the Daemon whose state directory the CLI would use, `$PROCLY_STATE_DIR` first, and of the
    /// `procly` beside the executable of the app.
    ///
    /// # Errors
    ///
    /// When no state directory can be resolved from the environment, or the executable of the app is unknown.
    pub(crate) fn resolve() -> Result<Self, SetupError> {
        let app = std::env::current_exe().map_err(SetupError::Executable)?;
        Ok(Self::new(StateDir::resolve()?, sidecar_beside(&app)))
    }

    pub(crate) fn state(&self) -> &StateDir {
        &self.state
    }

    /// The embedded `procly`.
    pub(crate) fn procly(&self) -> &Path {
        &self.procly
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

impl Source for DaemonClient {
    async fn reachable(&self) -> bool {
        let state = self.state.clone();
        // A refused connection is the answer, and a thread that broke tells nothing about the Daemon.
        tauri::async_runtime::spawn_blocking(move || connect(&state).is_ok())
            .await
            .unwrap_or(false)
    }

    async fn status(&self) -> Result<Response, CommandError> {
        DaemonClient::status(self).await
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
            tauri::async_runtime::block_on(DaemonClient::new(state, PathBuf::from("/p/bin/procly")).status())
                .expect("a running Daemon answers");

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

        let error = tauri::async_runtime::block_on(DaemonClient::new(state, PathBuf::from("/p/bin/procly")).status())
            .expect_err("no Daemon runs here");

        let json = serde_json::to_value(&error).expect("the error serializes");
        assert_eq!(json["kind"], "unreachable");
        // Starting a Daemon would leave its socket, lock, pid and log behind.
        let left: Vec<_> = std::fs::read_dir(dir.path())
            .expect("the directory is readable")
            .collect();
        assert!(left.is_empty(), "nothing is created in the state directory: {left:?}");
    }

    #[test]
    fn the_embedded_procly_is_the_file_beside_the_executable_of_the_app() {
        assert_eq!(
            sidecar_beside(Path::new("/Applications/Procly.app/Contents/MacOS/procly-desktop")),
            Path::new("/Applications/Procly.app/Contents/MacOS/procly"),
        );
    }

    #[test]
    fn a_daemon_of_another_protocol_version_reaches_the_window_with_both_versions() {
        let error = CommandError::Client(ClientError::Decode(DecodeError::Version(PROTOCOL_VERSION - 1)));

        let json = serde_json::to_value(&error).expect("the error serializes");

        assert_eq!(json["kind"], "protocol_mismatch");
        assert_eq!(json["daemon_protocol"], PROTOCOL_VERSION - 1);
        assert_eq!(json["app_protocol"], PROTOCOL_VERSION);
        assert!(json["message"].as_str().is_some_and(|message| !message.is_empty()));
    }

    #[test]
    fn an_unreadable_answer_stays_a_decode_failure() {
        let malformed = serde_json::from_str::<u32>("{").expect_err("the text is not JSON");
        let error = CommandError::Client(ClientError::Decode(DecodeError::Malformed(malformed)));

        let json = serde_json::to_value(&error).expect("the error serializes");

        assert_eq!(json["kind"], "decode");
        assert!(json.get("daemon_protocol").is_none());
    }
}
