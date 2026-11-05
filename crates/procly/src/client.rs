//! The CLI side of the Daemon socket: `procly-client`, starting the Daemon with this very executable when no one
//! answers.

use anyhow::Context as _;
use procly_client::protocol::{Request, Response};
use procly_client::state_dir::StateDir;
use procly_client::{ClientError, connect, connect_or_start};

/// Sends one request to the Daemon, starting it first if it is not running.
pub(crate) fn send(state: &StateDir, request: &Request) -> anyhow::Result<Response> {
    send_with_progress(state, request, |_| ())
}

/// Sends one request to the Daemon, starting it first if it is not running, handing each
/// [`Response::SetupProgress`] it sends ahead of its answer to `on_progress`, and returns the answer.
///
/// The Daemon this starts never sees the lifeline of a `procly mcp`, which [`crate::lifeline::watch`] already removed
/// from the environment of this process.
pub(crate) fn send_with_progress(
    state: &StateDir,
    request: &Request,
    on_progress: impl FnMut(&Response),
) -> anyhow::Result<Response> {
    // The executable is located only when a Daemon has to be started, as it always was.
    let connection = if let Ok(connection) = connect(state) {
        connection
    } else {
        let exe = std::env::current_exe().context("cannot locate the procly executable to start the daemon")?;
        connect_or_start(state, &exe)?
    };
    Ok(connection.send(request)?.answer(on_progress)?)
}

/// Sends one request to the Daemon only if one runs, and never starts it.
pub(crate) fn send_if_running(state: &StateDir, request: &Request) -> Result<Response, ClientError> {
    connect(state)?.send(request)?.answer(|_| ())
}
