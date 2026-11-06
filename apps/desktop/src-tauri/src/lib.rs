//! The Procly desktop app: one window, a client of the Daemon like the CLI (ADR 0009).
//!
//! Every command sends one `Request` through `procly-client` and returns the `Response` unchanged, the frontend
//! draws it. Nothing here supervises anything. The window is kept current by one poller (`docs/rules/tauri.md`).

mod apps;
mod atomic;
mod clipboard;
mod daemon;
mod dock;
mod doctor;
mod follow;
mod glyph;
mod lifecycle;
mod login_item;
mod menu;
mod needs_you;
mod notifications;
mod opening;
mod orphans;
mod poller;
mod services;
mod settings;
mod tray;
mod tray_menu;
mod window;

use std::sync::Mutex;

use daemon::DaemonClient;
use dock::DockIcon;
use follow::{Follower, WINDOW_VISIBLE};
use needs_you::{Attention, FailureKey, Presented, SeenError};
use notifications::Notifier;
use poller::{Poller, WindowState};
use settings::{OPEN_SETTINGS, SETTINGS_FILE, SettingsStore};
use tauri::menu::MenuEvent;
use tauri::{AppHandle, Emitter as _, Manager as _, Runtime, State, Window, WindowEvent};
use tauri_plugin_window_state::StateFlags;
use tray::Tray;
use window::{Host as _, MainWindow};

/// The event that carries each answer of the poller to the window.
const DAEMON_UPDATE: &str = "daemon-update";

/// The event that tells the window the menu refreshed, so that it runs doctor again as it does on its own Refresh.
const REFRESH_REQUESTED: &str = "refresh-requested";

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

/// Shows the banners an answer calls for. Every answer is followed, but a banner shows only while "Notify on failure"
/// is on.
fn notify(app: &AppHandle, notifier: &mut Notifier, presented: &Presented) {
    let enabled = app
        .try_state::<SettingsStore>()
        .is_some_and(|settings| settings.get().notify_on_failure);
    for banner in notifier.banners(presented, enabled) {
        notifications::show(app, &banner);
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
fn window_state<R: Runtime>(window: &Window<R>) -> WindowState {
    // A window that is being destroyed answers nothing: nobody looks at it.
    let flag = |read: tauri::Result<bool>| read.unwrap_or(false);
    WindowState::from_flags(
        flag(window.is_visible()),
        flag(window.is_minimized()),
        flag(window.is_focused()),
    )
}

/// Handles a choice in the native menu: Refresh asks the poller for an answer at once, Settings opens that screen.
fn on_menu_event(app: &AppHandle, event: &MenuEvent) {
    if menu::is_refresh(event) {
        if let Some(poller) = app.try_state::<Poller>() {
            poller.refresh();
        }
        // The window runs doctor again on a Refresh, and this one did not go through it.
        let _ = app.emit(REFRESH_REQUESTED, ());
    }
    if menu::is_settings(event) {
        // A window that is gone has nothing to be told, and Settings opens from the sidebar anyway.
        let _ = app.emit(OPEN_SETTINGS, ());
    }
}

/// Handles a window event of the app. Closing the window hides it instead of ending the app, which stays in the menu
/// bar until the developer quits it. Then every event that changes where the window stands is followed.
fn on_window_event(window: &Window, event: &WindowEvent) {
    if let WindowEvent::CloseRequested { api, .. } = event
        && window.label() == window::MAIN
    {
        api.prevent_close();
        window::close(&MainWindow(window.app_handle()));
    }
    follow_window(window, event);
}

/// Tells the poller and the follow about a window event that changes where the window stands. A window that is hidden,
/// minimized or closed cancels the follow, and the window starts it again from a fresh backlog once it comes back.
fn follow_window<R: Runtime>(window: &Window<R>, event: &WindowEvent) {
    let state = match event {
        WindowEvent::Focused(_) | WindowEvent::Resized(_) => window_state(window),
        WindowEvent::CloseRequested { .. } | WindowEvent::Destroyed => WindowState::Closed,
        _ => return,
    };
    if let Some(poller) = window.try_state::<Poller>() {
        poller.window(state);
    }
    // A hidden window cancels the follow, and the window starts it again from a fresh backlog once it comes back.
    if let Some(follower) = window.try_state::<Follower>()
        && let Some(visible) = follower.window(state)
    {
        // A window that is gone has nothing to be told.
        let _ = window.emit(WINDOW_VISIBLE, visible);
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
            // A click on a banner is reported only for banners shown after this, and none shows before the first answer.
            notifications::open_on_click(app.handle());
            tauri::async_runtime::spawn(async move {
                let mut notifier = Notifier::default();
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
                        notify(&handle, &mut notifier, &presented);
                    }
                }
            });
            app.manage(client);
            app.manage(poller);
            app.manage(attention);
            app.manage(Follower::new());
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
        // Banners are sent from Rust and the window is granted no permission of the plugin.
        .plugin(tauri_plugin_notification::init())
        .on_menu_event(|app, event| {
            on_menu_event(app, &event);
        })
        .on_window_event(on_window_event)
        .invoke_handler(tauri::generate_handler![
            last_update,
            mark_seen,
            refresh,
            follow::follow_logs,
            follow::stop_follow,
            follow::service_logs,
            lifecycle::start_daemon,
            lifecycle::restart_daemon,
            doctor::doctor,
            apps::detected_apps,
            opening::open_worktree,
            opening::reveal_worktree,
            orphans::list_orphans,
            orphans::delete_orphans,
            services::stop_service,
            services::restart_service,
            settings::get_settings,
            settings::save_settings,
            login_item::get_open_at_login,
            login_item::set_open_at_login,
            notifications::set_notify_on_failure,
            notifications::open_notification_settings,
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
    fn the_window_is_granted_no_notification_permission() {
        // Rust sends the banners and asks for the permission, so the window cannot notify of its own making.
        let notification: Vec<_> = granted()
            .into_iter()
            .filter(|name| name.starts_with("notification:"))
            .collect();

        assert!(notification.is_empty(), "{notification:?}");
    }

    #[test]
    fn launching_the_app_never_asks_for_the_permission_to_notify() {
        // The permission is asked when "Notify on failure" is turned on, which `notifications::switch` does.
        let source = include_str!("lib.rs");
        let launch = source
            .split("#[cfg(test)]")
            .next()
            .expect("the source before the tests");

        assert!(!launch.contains("request_permission"));
        assert!(!launch.contains("Permission"));
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
