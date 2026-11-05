//! The Procly desktop app: one window, a client of the Daemon like the CLI (ADR 0009).
//!
//! Every command sends one `Request` through `procly-client` and returns the `Response` unchanged, the frontend
//! draws it. Nothing here supervises anything. The window is kept current by one poller (`docs/rules/tauri.md`).

mod daemon;
mod menu;
mod poller;

use std::sync::Arc;

use daemon::DaemonClient;
use poller::{Poller, Update, WindowState};
use tauri::{Emitter as _, Manager as _, State, Window, WindowEvent};

/// The event that carries each answer of the poller to the window.
const DAEMON_UPDATE: &str = "daemon-update";

/// The last answer of the poller, so that a window that opens or reloads does not wait for the next tick.
#[tauri::command]
#[expect(
    clippy::needless_pass_by_value,
    reason = "Tauri hands managed state to a command as a `State`"
)]
fn last_update(poller: State<'_, Poller>) -> Option<Arc<Update>> {
    poller.last()
}

/// Asks the poller for a tick now. It never starts, stops or restarts anything.
#[tauri::command]
#[expect(
    clippy::needless_pass_by_value,
    reason = "Tauri hands managed state to a command as a `State`"
)]
fn refresh(poller: State<'_, Poller>) {
    poller.refresh();
}

/// Where the window stands, from what Tauri reports about it.
fn window_state(window: &Window) -> WindowState {
    // A window that is being destroyed answers nothing: nobody looks at it.
    let flag = |read: tauri::Result<bool>| read.unwrap_or(false);
    WindowState::from_flags(
        flag(window.is_visible()),
        flag(window.is_minimized()),
        flag(window.is_focused()),
    )
}

/// Tells the poller about a window event that changes where the window stands.
fn follow_window(window: &Window, event: &WindowEvent) {
    let state = match event {
        WindowEvent::Focused(_) | WindowEvent::Resized(_) => window_state(window),
        WindowEvent::CloseRequested { .. } | WindowEvent::Destroyed => WindowState::Closed,
        _ => return,
    };
    if let Some(poller) = window.try_state::<Poller>() {
        poller.window(state);
    }
}

/// Runs the app until its last window closes. Closing the window or quitting the app stops nothing in the Daemon.
///
/// # Panics
///
/// When Tauri cannot start, which leaves nothing to show.
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let client = DaemonClient::resolve()?;
            // The window opens in front of the others.
            let poller = Poller::start(client.clone(), WindowState::Front);
            let mut updates = poller.subscribe();
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                while updates.changed().await.is_ok() {
                    let update = updates.borrow_and_update().clone();
                    if let Some(update) = update {
                        // A window that is gone has nothing to be told, the next one asks `last_update`.
                        let _ = handle.emit(DAEMON_UPDATE, &*update);
                    }
                }
            });
            app.manage(client);
            app.manage(poller);
            app.set_menu(menu::build(app.handle())?)?;
            Ok(())
        })
        .on_menu_event(|app, event| {
            if menu::is_refresh(&event)
                && let Some(poller) = app.try_state::<Poller>()
            {
                poller.refresh();
            }
        })
        .on_window_event(follow_window)
        .invoke_handler(tauri::generate_handler![last_update, refresh])
        .run(tauri::generate_context!())
        .expect("the Tauri application runs");
}
