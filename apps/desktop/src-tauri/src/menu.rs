//! The native menu of the app: the Procly menu with its Settings and Refresh items, and the Edit menu a webview needs
//! for copy and paste.

use tauri::menu::{Menu, MenuEvent, MenuItemBuilder, SubmenuBuilder};
use tauri::{AppHandle, Wry};

/// The id of the Refresh item, which `⌘R` triggers.
const REFRESH: &str = "refresh";

/// The id of the Settings item, which `⌘,` triggers.
const SETTINGS: &str = "settings";

/// Whether a menu event is the Refresh item being chosen.
pub(crate) fn is_refresh(event: &MenuEvent) -> bool {
    event.id().as_ref() == REFRESH
}

/// Whether a menu event is the Settings item being chosen.
pub(crate) fn is_settings(event: &MenuEvent) -> bool {
    event.id().as_ref() == SETTINGS
}

/// The menu bar: on macOS the first submenu is the app menu, named after the app.
///
/// # Errors
///
/// When Tauri cannot build a native menu item.
pub(crate) fn build(app: &AppHandle) -> tauri::Result<Menu<Wry>> {
    let refresh = MenuItemBuilder::with_id(REFRESH, "Refresh")
        .accelerator("CmdOrCtrl+R")
        .build(app)?;
    let settings = MenuItemBuilder::with_id(SETTINGS, "Settings…")
        .accelerator("CmdOrCtrl+,")
        .build(app)?;
    let procly = SubmenuBuilder::new(app, "Procly")
        .about(None)
        .separator()
        .item(&settings)
        .item(&refresh)
        .separator()
        .hide()
        .hide_others()
        .show_all()
        .separator()
        .quit()
        .build()?;
    let edit = SubmenuBuilder::new(app, "Edit")
        .undo()
        .redo()
        .separator()
        .cut()
        .copy()
        .paste()
        .select_all()
        .build()?;
    Menu::with_items(app, &[&procly, &edit])
}
