//! The Procly desktop app: one window, a client of the Daemon like the CLI (ADR 0009).
//!
//! Every command sends one `Request` through `procly-client` and returns the `Response` unchanged, the frontend
//! draws it. Nothing here supervises anything. The window is kept current by one poller (`docs/rules/tauri.md`).

mod daemon;
mod menu;
mod needs_you;
mod poller;

use daemon::DaemonClient;
use needs_you::{Attention, FailureKey, Presented, SeenError};
use poller::{Poller, WindowState};
use tauri::{AppHandle, Emitter as _, Manager as _, State, Window, WindowEvent};

/// The event that carries each answer of the poller to the window.
const DAEMON_UPDATE: &str = "daemon-update";

/// The file the Seen failures survive a restart in, inside the data directory of the app.
const SEEN_FILE: &str = "seen.json";

/// The last answer of the poller, so that a window that opens or reloads does not wait for the next tick.
#[tauri::command]
#[expect(
    clippy::needless_pass_by_value,
    reason = "Tauri hands managed state to a command as a `State`"
)]
fn last_update(poller: State<'_, Poller>, attention: State<'_, Attention>) -> Option<Presented> {
    poller.last().map(|update| attention.present_update(update))
}

/// Marks an ended failure as Seen: it leaves "Needs you" and the count, and the window is told at once rather than
/// at the next tick. A key the last answer holds no failure for is ignored. It changes nothing in the Daemon.
///
/// # Errors
///
/// When the Seen set cannot be written. The mark holds until the app quits, and the window is told of it anyway.
#[tauri::command]
async fn mark_seen(
    key: FailureKey,
    app: AppHandle,
    poller: State<'_, Poller>,
    attention: State<'_, Attention>,
) -> Result<(), SeenError> {
    let Some(last) = poller.last() else {
        return Ok(());
    };
    let attention = attention.inner().clone();
    // Writing the file blocks, so it leaves the task that serves the window.
    let (presented, saved) = tauri::async_runtime::spawn_blocking(move || attention.mark(key, last))
        .await
        .map_err(|error| SeenError::task(&error))?;
    publish(&app, &presented);
    saved
}

/// Tells the window about an answer.
fn publish(app: &AppHandle, presented: &Presented) {
    // A window that is gone has nothing to be told, the next one asks `last_update`.
    let _ = app.emit(DAEMON_UPDATE, presented);
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
            // Told as soon as the window moves, but the first tick should already match how it opened.
            let opened = app
                .get_webview_window("main")
                .map_or(WindowState::Front, |window| window_state(&window.as_ref().window()));
            let attention = Attention::open(app.path().app_data_dir()?.join(SEEN_FILE));
            let poller = Poller::start(client.clone(), opened);
            let mut updates = poller.subscribe();
            let handle = app.handle().clone();
            let seen = attention.clone();
            tauri::async_runtime::spawn(async move {
                while updates.changed().await.is_ok() {
                    let update = updates.borrow_and_update().clone();
                    if let Some(update) = update {
                        let seen = seen.clone();
                        // Pruning the Seen set may write its file, which blocks.
                        let Ok((presented, saved)) =
                            tauri::async_runtime::spawn_blocking(move || seen.on_answer(update)).await
                        else {
                            continue;
                        };
                        if let Err(error) = saved {
                            eprintln!("procly: {error}");
                        }
                        publish(&handle, &presented);
                    }
                }
            });
            app.manage(client);
            app.manage(poller);
            app.manage(attention);
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
        .invoke_handler(tauri::generate_handler![last_update, mark_seen, refresh])
        .run(tauri::generate_context!())
        .expect("the Tauri application runs");
}
