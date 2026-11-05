//! Collecting Orphans: a dry run to list them, then a deletion limited to the ones the developer saw.
//!
//! The window can only ask for those 2 things. There is no command that sends `Gc { apply: true }` without `only`, so a
//! bare apply, which would delete what the sheet never showed, cannot be sent from the app (`docs/rules/tauri.md`).

use std::path::PathBuf;

use procly_client::protocol::{OrphanId, Request, Response};
use tauri::State;

use crate::daemon::{CommandError, DaemonClient};

impl DaemonClient {
    /// The Orphans of every Repository, with `Gc { apply: false }`. Nothing is deleted.
    ///
    /// # Errors
    ///
    /// As [`DaemonClient::send`].
    pub(crate) async fn list_orphans(&self) -> Result<Response, CommandError> {
        self.send(Request::Gc {
            // The directory only names the requester in the log of the Daemon.
            cwd: PathBuf::from("/"),
            apply: false,
            only: None,
        })
        .await
    }

    /// Deletes the Orphans of `only`, with `Gc { apply: true, only }`. The Daemon deletes those that still are
    /// Orphans and reports the others as not deleted, with the reason.
    ///
    /// # Errors
    ///
    /// As [`DaemonClient::send`].
    pub(crate) async fn delete_orphans(&self, only: Vec<OrphanId>) -> Result<Response, CommandError> {
        self.send(Request::Gc {
            cwd: PathBuf::from("/"),
            apply: true,
            only: Some(only),
        })
        .await
    }
}

/// Lists the Orphans, for the Orphans screen.
///
/// # Errors
///
/// See [`DaemonClient::list_orphans`].
#[tauri::command]
pub(crate) async fn list_orphans(client: State<'_, DaemonClient>) -> Result<Response, CommandError> {
    client.list_orphans().await
}

/// Deletes the Orphans the developer confirmed, the `orphans` of the last dry run sent back as returned.
///
/// # Errors
///
/// See [`DaemonClient::delete_orphans`].
#[tauri::command]
pub(crate) async fn delete_orphans(
    only: Vec<OrphanId>,
    client: State<'_, DaemonClient>,
) -> Result<Response, CommandError> {
    client.delete_orphans(only).await
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead as _, BufReader, Write as _};
    use std::os::unix::net::UnixListener;
    use std::thread::{self, JoinHandle};

    use procly_client::protocol::{self, GcReport, OrphanKind};
    use procly_client::state_dir::StateDir;

    use super::*;

    /// A stand-in Daemon that answers one request with an empty report, and hands back the line it was sent.
    fn daemon(state: &StateDir) -> JoinHandle<String> {
        let listener = UnixListener::bind(state.socket()).expect("the stand-in Daemon binds the socket");
        let answer = protocol::encode(Response::Gc(GcReport::default())).expect("the answer encodes");
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
            .prefix("pdo")
            .tempdir()
            .expect("the temporary directory can be created");
        let client = DaemonClient::new(StateDir::at(dir.path()), PathBuf::from("/p/bin/procly"));
        (dir, client)
    }

    fn orphan(worktree: &str) -> OrphanId {
        OrphanId {
            kind: OrphanKind::OutputDirectory {
                path: PathBuf::from("/p/state/output/web_login"),
            },
            repository: PathBuf::from("/p/work/acme/web/.git"),
            worktree: PathBuf::from(worktree),
        }
    }

    #[test]
    fn listing_asks_for_a_dry_run_limited_to_nothing() {
        let (_dir, client) = client();
        let daemon = daemon(client.state());

        let response = tauri::async_runtime::block_on(client.list_orphans()).expect("a running Daemon answers");

        assert!(matches!(response, Response::Gc(_)));
        let request = daemon.join().expect("the stand-in Daemon ends");
        assert!(request.contains(r#""request":"gc""#), "{request}");
        assert!(request.contains(r#""apply":false"#), "{request}");
        assert!(!request.contains("only"), "{request}");
    }

    #[test]
    fn deleting_applies_to_the_listed_orphans_only() {
        let (_dir, client) = client();
        let daemon = daemon(client.state());

        tauri::async_runtime::block_on(client.delete_orphans(vec![orphan("/p/work/acme/web-login")]))
            .expect("a running Daemon answers");

        let request = daemon.join().expect("the stand-in Daemon ends");
        assert!(request.contains(r#""apply":true"#), "{request}");
        assert!(request.contains(r#""only":[{"#), "{request}");
        assert!(request.contains(r#""worktree":"/p/work/acme/web-login""#), "{request}");
        assert!(request.contains(r#""kind":"output_directory""#), "{request}");
    }

    #[test]
    fn the_orphans_of_a_dry_run_are_accepted_back_as_returned() {
        // The window sends the `orphans` of the dry run unchanged, `deleted` and `error` included.
        let returned = serde_json::json!([{
            "kind": "lease",
            "service": "web",
            "port_name": "http",
            "port": 24081,
            "repository": "/p/work/acme/web/.git",
            "worktree": "/p/work/acme/web-login",
            "deleted": false,
            "error": null
        }]);

        let only: Vec<OrphanId> = serde_json::from_value(returned).expect("an Orphan is an `OrphanId` with extras");

        assert_eq!(only.len(), 1);
        assert_eq!(only[0].worktree, PathBuf::from("/p/work/acme/web-login"));
    }
}
