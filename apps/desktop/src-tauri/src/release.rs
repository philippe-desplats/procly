//! Releasing a Worktree: a dry run to read what it would do, then the real release, each one `Request` whose `Response`
//! comes back unchanged.
//!
//! The 2 are 2 commands, so that the window never decides the `dry_run` flag of a request at run time: the sheet that
//! opens can only ask for the dry run, and the real release is the command its Release button calls.

use std::path::PathBuf;

use procly_client::protocol::{Request, Response};
use tauri::State;

use crate::daemon::{CommandError, DaemonClient};

impl DaemonClient {
    /// What releasing the Worktree at `path` would do, with `WorktreeRelease { dry_run: true }`. Nothing changes.
    ///
    /// # Errors
    ///
    /// As [`DaemonClient::send`].
    pub(crate) async fn preview_release(&self, path: PathBuf) -> Result<Response, CommandError> {
        self.send(Request::WorktreeRelease { path, dry_run: true }).await
    }

    /// Releases the Worktree at `path`, with `WorktreeRelease { dry_run: false }`.
    ///
    /// # Errors
    ///
    /// As [`DaemonClient::send`].
    pub(crate) async fn release_worktree(&self, path: PathBuf) -> Result<Response, CommandError> {
        self.send(Request::WorktreeRelease { path, dry_run: false }).await
    }
}

/// Reads what releasing a Worktree would do, for the sheet that opens before a release.
///
/// # Errors
///
/// See [`DaemonClient::preview_release`].
#[tauri::command]
pub(crate) async fn preview_release(path: PathBuf, client: State<'_, DaemonClient>) -> Result<Response, CommandError> {
    client.preview_release(path).await
}

/// Releases a Worktree, once the developer confirmed the sheet.
///
/// # Errors
///
/// See [`DaemonClient::release_worktree`].
#[tauri::command]
pub(crate) async fn release_worktree(path: PathBuf, client: State<'_, DaemonClient>) -> Result<Response, CommandError> {
    client.release_worktree(path).await
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead as _, BufReader, Write as _};
    use std::os::unix::net::UnixListener;
    use std::thread::{self, JoinHandle};

    use procly_client::protocol::{self, ReleaseReport};
    use procly_client::state_dir::StateDir;
    use tauri::Manager as _;
    use tauri::test::{mock_builder, mock_context, noop_assets};

    use super::*;

    /// A stand-in Daemon that answers one request with an empty report, and hands back the line it was sent.
    fn daemon(state: &StateDir) -> JoinHandle<String> {
        let listener = UnixListener::bind(state.socket()).expect("the stand-in Daemon binds the socket");
        let answer = protocol::encode(Response::WorktreeRelease(ReleaseReport::default())).expect("the answer encodes");
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
            .prefix("pdr")
            .tempdir()
            .expect("the temporary directory can be created");
        let client = DaemonClient::new(StateDir::at(dir.path()), PathBuf::from("/p/bin/procly"));
        (dir, client)
    }

    #[test]
    fn previewing_asks_for_a_dry_run_of_the_worktree() {
        let (_dir, client) = client();
        let daemon = daemon(client.state());

        let response = tauri::async_runtime::block_on(client.preview_release(PathBuf::from("/p/work/acme/web-fix")))
            .expect("a running Daemon answers");

        assert!(matches!(response, Response::WorktreeRelease(_)));
        let request = daemon.join().expect("the stand-in Daemon ends");
        assert!(request.contains(r#""request":"worktree_release""#), "{request}");
        assert!(request.contains(r#""path":"/p/work/acme/web-fix""#), "{request}");
        assert!(request.contains(r#""dry_run":true"#), "{request}");
    }

    #[test]
    fn releasing_asks_for_the_real_release_of_the_worktree() {
        let (_dir, client) = client();
        let daemon = daemon(client.state());

        tauri::async_runtime::block_on(client.release_worktree(PathBuf::from("/p/work/acme/web-fix")))
            .expect("a running Daemon answers");

        let request = daemon.join().expect("the stand-in Daemon ends");
        assert!(request.contains(r#""request":"worktree_release""#), "{request}");
        assert!(request.contains(r#""path":"/p/work/acme/web-fix""#), "{request}");
        assert!(request.contains(r#""dry_run":false"#), "{request}");
    }

    /// A mock app that manages `client`, so that a command runs the way the window runs it: against the managed state.
    fn app_managing(client: DaemonClient) -> tauri::App<tauri::test::MockRuntime> {
        let app = mock_builder()
            .build(mock_context(noop_assets()))
            .expect("the mock app builds");
        app.manage(client);
        app
    }

    #[test]
    fn the_release_command_sends_the_real_release_and_not_the_dry_run() {
        let (_dir, client) = client();
        let daemon = daemon(client.state());

        let app = app_managing(client);
        let path = PathBuf::from("/p/work/acme/web-fix");

        let response = tauri::async_runtime::block_on(release_worktree(path, app.state::<DaemonClient>()))
            .expect("a running Daemon answers");

        assert!(matches!(response, Response::WorktreeRelease(_)));

        let request = daemon.join().expect("the stand-in Daemon ends");
        assert!(request.contains(r#""path":"/p/work/acme/web-fix""#), "{request}");
        assert!(request.contains(r#""dry_run":false"#), "{request}");
    }

    #[test]
    fn the_preview_command_sends_the_dry_run_and_not_the_real_release() {
        let (_dir, client) = client();
        let daemon = daemon(client.state());

        let app = app_managing(client);
        let path = PathBuf::from("/p/work/acme/web-fix");

        let response = tauri::async_runtime::block_on(preview_release(path, app.state::<DaemonClient>()))
            .expect("a running Daemon answers");

        assert!(matches!(response, Response::WorktreeRelease(_)));

        let request = daemon.join().expect("the stand-in Daemon ends");
        assert!(request.contains(r#""path":"/p/work/acme/web-fix""#), "{request}");
        assert!(request.contains(r#""dry_run":true"#), "{request}");
    }
}
