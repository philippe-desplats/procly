//! The client side of the Daemon socket: a connection carries one request, then the stream of responses to it.

use std::fs::OpenOptions;
use std::io::{self, BufRead as _, BufReader, Write as _};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Serialize, Serializer};

use crate::protocol::{self, DecodeError, Request, Response};
use crate::state_dir::{StateDir, StateDirError};

/// How long [`connect_or_start`] waits for a Daemon it just started to accept connections.
const START_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error(transparent)]
    StateDir(#[from] StateDirError),
    #[error("cannot open the daemon log {path}")]
    OpenLog {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
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

/// Which [`ClientError`] a [`ClientFailure`] stands for, so that a client in another language can tell a Daemon that
/// does not run from one that answers badly without reading the message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub enum ClientFailureKind {
    StateDir,
    OpenLog,
    StartDaemon,
    Unreachable,
    Connection,
    Encode,
    NoResponse,
    Decode,
}

/// A [`ClientError`] as it crosses a process boundary, such as the one between the Rust side of the desktop app and
/// its webview. The source chain stays on this side, the message is the top level one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct ClientFailure {
    pub kind: ClientFailureKind,
    pub message: String,
}

impl ClientError {
    /// The kind of this error, as a [`ClientFailure`] carries it.
    #[must_use]
    pub fn kind(&self) -> ClientFailureKind {
        match self {
            Self::StateDir(_) => ClientFailureKind::StateDir,
            Self::OpenLog { .. } => ClientFailureKind::OpenLog,
            Self::StartDaemon { .. } => ClientFailureKind::StartDaemon,
            Self::Unreachable { .. } => ClientFailureKind::Unreachable,
            Self::Connection { .. } => ClientFailureKind::Connection,
            Self::Encode(_) => ClientFailureKind::Encode,
            Self::NoResponse => ClientFailureKind::NoResponse,
            Self::Decode(_) => ClientFailureKind::Decode,
        }
    }
}

impl Serialize for ClientError {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        ClientFailure {
            kind: self.kind(),
            message: self.to_string(),
        }
        .serialize(serializer)
    }
}

/// A connection to the Daemon, ready to carry one request.
#[derive(Debug)]
pub struct Connection {
    stream: UnixStream,
    socket: PathBuf,
}

/// Connects to the Daemon if one runs, and never starts it: starting a Daemon reconciles what the previous one left,
/// and a caller that promises to change nothing cannot have that effect.
///
/// # Errors
///
/// [`ClientError::Unreachable`] when no Daemon accepts the connection.
pub fn connect(state: &StateDir) -> Result<Connection, ClientError> {
    let socket = state.socket();
    match UnixStream::connect(&socket) {
        Ok(stream) => Ok(Connection { stream, socket }),
        Err(source) => Err(ClientError::Unreachable { path: socket, source }),
    }
}

/// Connects to the Daemon, starting it first with `exe daemon` if no one answers. `exe` is the `procly` executable
/// the caller vouches for, never guessed here: the CLI passes itself, the desktop app the copy it embeds.
///
/// # Errors
///
/// When the state directory or the log of the Daemon cannot be created, when `exe` cannot be started, or when the
/// Daemon it started does not accept connections within 5 seconds.
pub fn connect_or_start(state: &StateDir, exe: &Path) -> Result<Connection, ClientError> {
    if let Ok(connection) = connect(state) {
        return Ok(connection);
    }
    start_daemon(state, exe)?;
    // Several callers may start a Daemon at once, the one holding the lock serves them all.
    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        match connect(state) {
            Ok(connection) => return Ok(connection),
            Err(error) if Instant::now() >= deadline => return Err(error),
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

impl Connection {
    /// Sends `request`, the one request of this connection, and returns the stream of responses to it.
    ///
    /// # Errors
    ///
    /// When `request` cannot be encoded or the connection breaks while it is written.
    pub fn send(self, request: &Request) -> Result<Responses, ClientError> {
        let line = protocol::encode(request).map_err(ClientError::Encode)?;
        (&self.stream)
            .write_all(line.as_bytes())
            .map_err(|source| ClientError::Connection {
                path: self.socket.clone(),
                source,
            })?;
        Ok(Responses {
            reader: BufReader::new(self.stream),
            socket: self.socket,
            ended: false,
        })
    }
}

/// The responses of the Daemon to one request, one per line, in the order it sent them. The stream ends when the
/// Daemon closes the connection, or after the first error. Dropping it closes the connection.
#[derive(Debug)]
pub struct Responses {
    reader: BufReader<UnixStream>,
    socket: PathBuf,
    ended: bool,
}

/// A handle that closes the connection of a [`Responses`] from another thread, which ends the stream even while a
/// reader waits on a Daemon that says nothing. It is how a caller cancels a followed request.
#[derive(Debug)]
pub struct Closer {
    stream: UnixStream,
}

impl Closer {
    /// Closes both halves of the connection. The reader blocked on it sees the end of the stream, and the Daemon sees
    /// its client gone. Closing a connection already closed does nothing.
    pub fn close(&self) {
        // A connection the Daemon already closed has nothing left to shut down.
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

impl Responses {
    /// A handle that closes this connection from another thread.
    ///
    /// # Errors
    ///
    /// When the socket cannot be duplicated.
    pub fn closer(&self) -> io::Result<Closer> {
        Ok(Closer {
            stream: self.reader.get_ref().try_clone()?,
        })
    }

    /// Returns the answer, the first response that is not a [`Response::SetupProgress`], handing each one sent ahead
    /// of it to `on_progress`.
    ///
    /// # Errors
    ///
    /// [`ClientError::NoResponse`] when the Daemon closes the connection before answering, and any error of the
    /// stream.
    pub fn answer(self, mut on_progress: impl FnMut(&Response)) -> Result<Response, ClientError> {
        for response in self {
            let response = response?;
            if matches!(response, Response::SetupProgress { .. }) {
                on_progress(&response);
            } else {
                return Ok(response);
            }
        }
        Err(ClientError::NoResponse)
    }

    fn read(&mut self) -> Option<Result<Response, ClientError>> {
        let mut line = String::new();
        match self.reader.read_line(&mut line) {
            Ok(0) => None,
            Ok(_) => Some(protocol::decode(&line).map_err(ClientError::from)),
            Err(source) => Some(Err(ClientError::Connection {
                path: self.socket.clone(),
                source,
            })),
        }
    }
}

impl Iterator for Responses {
    type Item = Result<Response, ClientError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.ended {
            return None;
        }
        let next = self.read();
        self.ended = !matches!(next, Some(Ok(_)));
        next
    }
}

fn start_daemon(state: &StateDir, exe: &Path) -> Result<(), ClientError> {
    state.create()?;
    let open_error = |source| ClientError::OpenLog {
        path: state.log(),
        source,
    };
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(state.log())
        .map_err(open_error)?;
    let log_err = log.try_clone().map_err(open_error)?;

    // The Daemon must not inherit the stdout and stderr of the caller: it outlives it, and whoever reads the output of
    // the caller until end of file would hang on it. It detaches into its own session, and init reaps it once the
    // caller has exited, which is why the child handle is not waited for.
    Command::new(exe)
        .arg("daemon")
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(log_err)
        .spawn()
        .map_err(|source| ClientError::StartDaemon {
            exe: exe.to_path_buf(),
            source,
        })?;
    Ok(())
}
