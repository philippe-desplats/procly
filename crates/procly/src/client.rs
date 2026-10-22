//! The client side of the Daemon socket, starting the Daemon when no one answers.

use std::fs::OpenOptions;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::protocol::{self, DecodeError, Request, Response};
use crate::state_dir::{StateDir, StateDirError};

/// How long a command waits for a Daemon it just started to accept connections.
const START_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub(crate) enum ClientError {
    #[error(transparent)]
    StateDir(#[from] StateDirError),
    #[error("cannot open the daemon log {path}")]
    OpenLog {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cannot locate the procly executable to start the daemon")]
    LocateExe(#[source] io::Error),
    #[error("cannot start the daemon with {exe} daemon")]
    StartDaemon {
        exe: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("the daemon did not answer on {path}")]
    Unreachable {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("lost the connection to the daemon on {path}")]
    Connection {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cannot send the request, a path in it is not valid UTF-8")]
    Encode(#[source] serde_json::Error),
    #[error("the daemon closed the connection without answering")]
    NoResponse,
    #[error("unreadable answer from the daemon")]
    Decode(#[from] DecodeError),
}

/// Sends one request to the Daemon, starting it first if it is not running.
pub(crate) fn send(state: &StateDir, request: &Request) -> Result<Response, ClientError> {
    send_with_progress(state, request, |_| ())
}

/// Sends one request to the Daemon, handing each [`Response::SetupProgress`] it sends ahead of its answer to
/// `on_progress`, and returns the answer.
pub(crate) fn send_with_progress(
    state: &StateDir,
    request: &Request,
    on_progress: impl FnMut(&Response),
) -> Result<Response, ClientError> {
    let stream = connect_or_start(state)?;
    exchange(state, request, &stream, on_progress)
}

/// Sends one request to the Daemon only if one runs, and never starts it: starting a Daemon reconciles what the
/// previous one left, and a command that promises to change nothing cannot have that effect.
pub(crate) fn send_if_running(state: &StateDir, request: &Request) -> Result<Response, ClientError> {
    let stream = UnixStream::connect(state.socket()).map_err(|source| ClientError::Unreachable {
        path: state.socket(),
        source,
    })?;
    exchange(state, request, &stream, |_| ())
}

fn exchange(
    state: &StateDir,
    request: &Request,
    stream: &UnixStream,
    mut on_progress: impl FnMut(&Response),
) -> Result<Response, ClientError> {
    let request = protocol::encode(request).map_err(ClientError::Encode)?;
    let connection_error = |source| ClientError::Connection {
        path: state.socket(),
        source,
    };

    (&*stream).write_all(request.as_bytes()).map_err(connection_error)?;
    let mut reader = BufReader::new(stream);
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).map_err(connection_error)?;
        if line.is_empty() {
            return Err(ClientError::NoResponse);
        }
        let response = protocol::decode(&line)?;
        if matches!(response, Response::SetupProgress { .. }) {
            on_progress(&response);
        } else {
            return Ok(response);
        }
    }
}

fn connect_or_start(state: &StateDir) -> Result<UnixStream, ClientError> {
    if let Ok(stream) = UnixStream::connect(state.socket()) {
        return Ok(stream);
    }
    start_daemon(state)?;
    // Several commands may start a Daemon at once, the one holding the lock serves them all.
    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        match UnixStream::connect(state.socket()) {
            Ok(stream) => return Ok(stream),
            Err(source) if Instant::now() >= deadline => {
                return Err(ClientError::Unreachable {
                    path: state.socket(),
                    source,
                });
            }
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

fn start_daemon(state: &StateDir) -> Result<(), ClientError> {
    state.create()?;
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(state.log())
        .map_err(|source| ClientError::OpenLog {
            path: state.log(),
            source,
        })?;
    let log_err = log.try_clone().map_err(|source| ClientError::OpenLog {
        path: state.log(),
        source,
    })?;
    let exe = std::env::current_exe().map_err(ClientError::LocateExe)?;

    // The Daemon must not inherit our stdout and stderr: it outlives this command, and a caller reading our
    // output until end of file would hang on it. It detaches into its own session, and init reaps it once this
    // short-lived command has exited, which is why the child handle is not waited for.
    Command::new(&exe)
        .arg("daemon")
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(log_err)
        .spawn()
        .map_err(|source| ClientError::StartDaemon { exe, source })?;
    Ok(())
}
