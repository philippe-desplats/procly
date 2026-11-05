//! The Procly desktop app: one window, a client of the Daemon like the CLI (ADR 0009).
//!
//! Every command sends one `Request` through `procly-client` and returns the `Response` unchanged, the frontend
//! draws it. Nothing here supervises anything.

mod daemon;

use daemon::{CommandError, DaemonClient};
use procly_client::protocol::Response;
use tauri::{Manager as _, State};

/// The status of every Worktree the Daemon knows, as the Daemon answered it.
#[tauri::command]
async fn status(client: State<'_, DaemonClient>) -> Result<Response, CommandError> {
    client.status().await
}

/// Runs the app until its last window closes.
///
/// # Panics
///
/// When Tauri cannot start, which leaves nothing to show.
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            app.manage(DaemonClient::resolve()?);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![status])
        .run(tauri::generate_context!())
        .expect("the Tauri application runs");
}
