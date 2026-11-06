//! Stopping and restarting one Service of a Worktree: each is one `Request` and the `Response` comes back unchanged.
//!
//! The window can ask for those 2 things and nothing that starts a Service. There is no command that sends `Up`:
//! a Service starts again only through `Restart`, which replays the environment the Daemon holds (`docs/rules/tauri.md`).

use std::path::PathBuf;

use procly_client::protocol::{Request, Response};
use tauri::State;

use crate::daemon::{CommandError, DaemonClient};

impl DaemonClient {
    /// Stops `service` of the Worktree at `cwd`, with `Down { service: Some(service) }`.
    ///
    /// # Errors
    ///
    /// As [`DaemonClient::send`].
    pub(crate) async fn stop_service(&self, service: String, cwd: PathBuf) -> Result<Response, CommandError> {
        self.send(Request::Down {
            service: Some(service),
            cwd,
        })
        .await
    }

    /// Stops `service` of the Worktree at `cwd` if it runs and starts it again, with `Restart`.
    ///
    /// # Errors
    ///
    /// As [`DaemonClient::send`].
    pub(crate) async fn restart_service(&self, service: String, cwd: PathBuf) -> Result<Response, CommandError> {
        self.send(Request::Restart { service, cwd }).await
    }
}

/// Stops a Service or an Isolated Compose service of a Worktree, with no confirmation.
///
/// # Errors
///
/// See [`DaemonClient::stop_service`].
#[tauri::command]
pub(crate) async fn stop_service(
    service: String,
    cwd: PathBuf,
    client: State<'_, DaemonClient>,
) -> Result<Response, CommandError> {
    client.stop_service(service, cwd).await
}

/// Restarts a Service with the environment of its last `up`. The window offers it only when the Service is
/// `restartable`.
///
/// # Errors
///
/// See [`DaemonClient::restart_service`].
#[tauri::command]
pub(crate) async fn restart_service(
    service: String,
    cwd: PathBuf,
    client: State<'_, DaemonClient>,
) -> Result<Response, CommandError> {
    client.restart_service(service, cwd).await
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead as _, BufReader, Write as _};
    use std::os::unix::net::UnixListener;
    use std::thread::{self, JoinHandle};

    use procly_client::protocol;
    use procly_client::state_dir::StateDir;

    use super::*;

    /// A stand-in Daemon that answers one request with `answer`, and hands back the line it was sent.
    fn daemon(state: &StateDir, answer: Response) -> JoinHandle<String> {
        let listener = UnixListener::bind(state.socket()).expect("the stand-in Daemon binds the socket");
        let answer = protocol::encode(answer).expect("the answer encodes");
        thread::spawn(move || {
            let (stream, _) = listener.accept().expect("the app connects");
            let mut request = String::new();
            BufReader::new(&stream)
                .read_line(&mut request)
                .expect("the request line is read");
            (&stream).write_all(answer.as_bytes()).expect("the answer is written");
            request
        })
    }

    fn client() -> (tempfile::TempDir, DaemonClient) {
        let dir = tempfile::Builder::new()
            .prefix("pdv")
            .tempdir()
            .expect("the temporary directory can be created");
        let client = DaemonClient::new(StateDir::at(dir.path()), PathBuf::from("/p/bin/procly"));
        (dir, client)
    }

    #[test]
    fn stopping_names_the_service_and_the_worktree() {
        let (_dir, client) = client();
        let daemon = daemon(
            client.state(),
            Response::Down {
                stopped: vec!["web".to_owned()],
                released: Vec::new(),
            },
        );

        let response = tauri::async_runtime::block_on(
            client.stop_service("web".to_owned(), PathBuf::from("/p/work/acme/web-fix")),
        )
        .expect("a running Daemon answers");

        assert!(matches!(response, Response::Down { .. }));
        let request = daemon.join().expect("the stand-in Daemon ends");
        assert!(request.contains(r#""request":"down""#), "{request}");
        assert!(request.contains(r#""service":"web""#), "{request}");
        assert!(request.contains(r#""cwd":"/p/work/acme/web-fix""#), "{request}");
    }

    #[test]
    fn restarting_names_the_service_and_the_worktree_and_sends_no_up() {
        let (_dir, client) = client();
        let daemon = daemon(
            client.state(),
            Response::Error {
                message: "web has no environment to restart with".to_owned(),
            },
        );

        let response = tauri::async_runtime::block_on(
            client.restart_service("web".to_owned(), PathBuf::from("/p/work/acme/web-fix")),
        )
        .expect("a running Daemon answers");

        // The refusal of the Daemon reaches the window unchanged.
        assert!(matches!(response, Response::Error { .. }));
        let request = daemon.join().expect("the stand-in Daemon ends");
        assert!(request.contains(r#""request":"restart""#), "{request}");
        assert!(request.contains(r#""service":"web""#), "{request}");
        assert!(request.contains(r#""cwd":"/p/work/acme/web-fix""#), "{request}");
        assert!(!request.contains(r#""request":"up""#), "{request}");
    }
}
