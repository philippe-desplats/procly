//! The Procly desktop app: one window, a client of the Daemon like the CLI (ADR 0009).
//!
//! Every command sends one `Request` through `procly-client` and returns the `Response` unchanged, the frontend
//! draws it. Nothing here supervises anything. The window is kept current by one poller (`docs/rules/tauri.md`).

mod apps;
mod atomic;
mod clipboard;
mod daemon;
mod dock;
mod glyph;
mod lifecycle;
mod login_item;
mod menu;
mod needs_you;
mod opening;
mod orphans;
mod poller;
mod settings;
mod tray;
mod tray_menu;
mod window;

use std::sync::Mutex;

use daemon::DaemonClient;
use dock::DockIcon;
use needs_you::{Attention, FailureKey, Presented, SeenError};
use poller::{Poller, WindowState};
use settings::{OPEN_SETTINGS, SETTINGS_FILE, SettingsStore};
use tauri::{AppHandle, Emitter as _, Manager as _, State, Window, WindowEvent};
use tauri_plugin_window_state::StateFlags;
use tray::Tray;
use window::{Host as _, MainWindow};

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

/// Tells the window and the menu bar icon about an answer.
fn publish(app: &AppHandle, presented: &Presented) {
    // A window that is gone has nothing to be told, the next one asks `last_update`.
    let _ = app.emit(DAEMON_UPDATE, presented);
    if let Some(tray) = app.try_state::<Tray>() {
        tray.show(app, presented);
    }
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

/// Tells the poller about a window event that changes where the window stands. Closing the window hides it instead of
/// ending the app, which stays in the menu bar until the developer quits it.
fn follow_window(window: &Window, event: &WindowEvent) {
    if let WindowEvent::CloseRequested { api, .. } = event
        && window.label() == window::MAIN
    {
        api.prevent_close();
        window::close(&MainWindow(window.app_handle()));
    }
    let state = match event {
        WindowEvent::Focused(_) | WindowEvent::Resized(_) => window_state(window),
        WindowEvent::CloseRequested { .. } | WindowEvent::Destroyed => WindowState::Closed,
        _ => return,
    };
    if let Some(poller) = window.try_state::<Poller>() {
        poller.window(state);
    }
}

#[cfg(all(feature = "e2e", not(debug_assertions)))]
compile_error!("the `e2e` feature opens a WebDriver server and must never be part of a release build (ADR 0016)");

/// The builder every run starts from. A build made for the end-to-end tests (ADR 0016) also carries the plugin that
/// serves `WebDriver` on the loopback port the test runner chose, which no other build has.
fn builder() -> tauri::Builder<tauri::Wry> {
    let builder = tauri::Builder::default();
    #[cfg(feature = "e2e")]
    let builder = builder.plugin(tauri_plugin_wdio_webdriver::init());
    builder
}

/// Runs the app until the developer quits it. Closing the window leaves it in the menu bar, and neither closing the
/// window nor quitting the app stops anything in the Daemon.
///
/// # Panics
///
/// When Tauri cannot start, which leaves nothing to show.
pub fn run() {
    builder()
        .setup(|app| {
            let client = DaemonClient::resolve()?;
            // Before the first change of the Dock icon, which tao may ignore when it follows a show too closely.
            app.manage(Mutex::new(DockIcon::default()));
            let shown = MainWindow(app.handle());
            // `LSUIElement` starts a bundled app with no Dock icon, and this does the same when it runs unbundled.
            shown.set_dock_visibility(false);
            app.manage(SettingsStore::open(app.path().app_config_dir()?.join(SETTINGS_FILE)));
            // Open at login starts the app hidden: no window, only the glyph, and no Daemon, which the app never starts
            // by itself. Any other launch opens the window.
            let hidden = window::launched_hidden(std::env::args().skip(1));
            if !hidden {
                shown.open();
            }
            // Told as soon as the window moves, but the first tick should already match how it opened.
            let opened = if hidden {
                WindowState::Closed
            } else {
                WindowState::Front
            };
            let attention = Attention::open(app.path().app_data_dir()?.join(SEEN_FILE));
            let poller = Poller::start(client.clone(), opened);
            app.manage(Tray::create(app.handle())?);
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
        // The window reopens where it was left, with no visible setting. Whether it shows is not part of what is
        // restored: Open at login starts the app hidden, and the app decides at launch.
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_state_flags(StateFlags::all() & !StateFlags::VISIBLE)
                .build(),
        )
        .plugin(login_item::plugin())
        .on_menu_event(|app, event| {
            if menu::is_refresh(&event)
                && let Some(poller) = app.try_state::<Poller>()
            {
                poller.refresh();
            }
            if menu::is_settings(&event) {
                // A window that is gone has nothing to be told, and Settings opens from the sidebar anyway.
                let _ = app.emit(OPEN_SETTINGS, ());
            }
        })
        .on_window_event(follow_window)
        .invoke_handler(tauri::generate_handler![
            last_update,
            mark_seen,
            refresh,
            lifecycle::start_daemon,
            lifecycle::restart_daemon,
            apps::detected_apps,
            opening::open_worktree,
            opening::reveal_worktree,
            orphans::list_orphans,
            orphans::delete_orphans,
            settings::get_settings,
            settings::save_settings,
            login_item::get_open_at_login,
            login_item::set_open_at_login,
        ])
        .build(tauri::generate_context!())
        .expect("the Tauri application builds")
        .run(|app, event| {
            // Clicking the Dock icon or opening the app again brings the window back.
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen { .. } = event {
                MainWindow(app).open();
            }
            #[cfg(not(target_os = "macos"))]
            let _ = (app, event);
        });
}

#[cfg(test)]
mod tests {
    /// The permissions the capability of the window lists, whether they are written as a name or with a scope.
    fn granted() -> Vec<String> {
        let capability: serde_json::Value =
            serde_json::from_str(include_str!("../capabilities/main-window.json")).expect("a capability file");
        capability["permissions"]
            .as_array()
            .expect("a list of permissions")
            .iter()
            .map(|permission| {
                permission
                    .as_str()
                    .or_else(|| permission["identifier"].as_str())
                    .expect("a permission is a name or has an identifier")
                    .to_owned()
            })
            .collect()
    }

    #[test]
    fn the_window_is_granted_no_opener_permission() {
        // Rust opens a Worktree, so the window cannot open a path or a URL of its own choosing.
        let opener: Vec<_> = granted()
            .into_iter()
            .filter(|name| name.starts_with("opener:"))
            .collect();

        assert!(opener.is_empty(), "{opener:?}");
    }

    #[test]
    fn the_window_is_granted_no_autostart_permission() {
        // Rust switches Open at login through 2 commands, so the window cannot write a LaunchAgent of its own making.
        let autostart: Vec<_> = granted()
            .into_iter()
            .filter(|name| name.starts_with("autostart:"))
            .collect();

        assert!(autostart.is_empty(), "{autostart:?}");
    }

    #[test]
    fn the_window_starts_hidden_so_that_the_app_decides_at_launch_whether_it_opens() {
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).expect("the Tauri configuration");

        assert_eq!(config["app"]["windows"][0]["visible"], false);
        assert_eq!(config["app"]["windows"][0]["label"], "main");
    }

    #[test]
    fn the_window_is_granted_nothing_to_move_or_restore_itself() {
        // The window reopens where it was left through the plugin, which needs no call from the window.
        let state: Vec<_> = granted()
            .into_iter()
            .filter(|name| name.starts_with("window-state:"))
            .collect();

        assert!(state.is_empty(), "{state:?}");
    }
}
