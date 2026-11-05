//! The menu bar extra: a tray icon with a native menu, fed by the poller. Never a webview popover.
//!
//! What the menu says and which glyph shows are decided in `tray_menu.rs` and `glyph.rs`, which are tested. This file
//! only draws their result and runs what the developer chooses, so a closed window still lets them see what needs them.

use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use tauri::image::Image;
use tauri::menu::{Menu, MenuItem, MenuItemBuilder, PredefinedMenuItem, SubmenuBuilder};
use tauri::tray::{TrayIcon, TrayIconBuilder};
use tauri::{AppHandle, Emitter as _, Wry};

use crate::clipboard;
use crate::glyph::{Glyph, GlyphTracker};
use crate::needs_you::Presented;
use crate::settings::OPEN_SETTINGS;
use crate::tray_menu::{self, Action, Model, Snapshot};
use crate::window::MainWindow;

/// The id of the one tray icon.
const TRAY_ID: &str = "procly";

/// The header of a menu, which is only text and does nothing when chosen.
const HEADER_ID: &str = "tray-header";

/// What is on screen: the glyph, the model the native items were built from, and the header to change in place.
struct Drawn {
    glyph: Glyph,
    model: Model,
    header: MenuItem<Wry>,
}

struct State {
    tracker: GlyphTracker,
    /// The last `Status` answer, which the menu keeps showing when the Daemon stops answering.
    last: Option<Snapshot>,
    drawn: Drawn,
}

/// The tray icon of the app, managed by Tauri. It is told about each answer of the poller.
pub(crate) struct Tray {
    icon: TrayIcon,
    state: Mutex<State>,
}

impl std::fmt::Debug for Tray {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Tray").finish_non_exhaustive()
    }
}

/// An absolute time of the machine, which is what the menu shows next to the last state.
fn local(at_ms: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| u64::try_from(since.as_millis()).unwrap_or(u64::MAX));
    tray_menu::moment(at_ms, now, &chrono::Local)
}

fn image(glyph: Glyph) -> tauri::Result<Image<'static>> {
    Image::from_bytes(glyph.png())
}

/// The native menu of `model`, and its header.
fn menu(app: &AppHandle, model: &Model) -> tauri::Result<(Menu<Wry>, MenuItem<Wry>)> {
    let header = MenuItemBuilder::with_id(HEADER_ID, &model.header)
        .enabled(false)
        .build(app)?;
    let menu = Menu::new(app)?;
    menu.append(&header)?;
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    for (index, line) in model.needs_you.iter().enumerate() {
        menu.append(&MenuItemBuilder::with_id(tray_menu::needs_you_id(index), line).build(app)?)?;
    }
    if !model.needs_you.is_empty() {
        menu.append(&PredefinedMenuItem::separator(app)?)?;
    }
    let mut copy_port = SubmenuBuilder::new(app, "Copy Port").enabled(!model.ports.is_empty());
    for entry in &model.ports {
        copy_port =
            copy_port.item(&MenuItemBuilder::with_id(tray_menu::copy_port_id(entry.port), &entry.label).build(app)?);
    }
    menu.append(&copy_port.build()?)?;
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    menu.append(&MenuItemBuilder::with_id(tray_menu::OPEN_ID, "Open Procly").build(app)?)?;
    menu.append(&MenuItemBuilder::with_id(tray_menu::SETTINGS_ID, "Settings…").build(app)?)?;
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    menu.append(&MenuItemBuilder::with_id(tray_menu::QUIT_ID, "Quit Procly").build(app)?)?;
    Ok((menu, header))
}

/// Runs what the developer chose in the menu. An id that is not an item of this menu is left to its owner.
fn run(app: &AppHandle, id: &str) {
    let Some(action) = tray_menu::action(id) else {
        return;
    };
    match action {
        Action::Open => MainWindow(app).open(),
        Action::Settings => {
            MainWindow(app).open();
            // The window listens while it is hidden too, and asks for the screen when it shows.
            let _ = app.emit(OPEN_SETTINGS, ());
        }
        // Quitting leaves the Daemon and every Service running.
        Action::Quit => app.exit(0),
        Action::CopyPort(port) => {
            if let Err(error) = clipboard::copy(&port.to_string()) {
                eprintln!("procly: cannot copy the port {port}: {error}");
            }
        }
    }
}

impl Tray {
    /// Puts the icon in the menu bar with the mark, which is what shows until the first answer fails.
    ///
    /// # Errors
    ///
    /// When Tauri cannot build the icon or its menu.
    pub(crate) fn create(app: &AppHandle) -> tauri::Result<Self> {
        let tracker = GlyphTracker::launched();
        let model = tray_menu::model(tracker.shown(), None, local);
        let (native, header) = menu(app, &model)?;
        let icon = TrayIconBuilder::with_id(TRAY_ID)
            .icon(image(tracker.shown())?)
            .icon_as_template(true)
            .menu(&native)
            .show_menu_on_left_click(true)
            .tooltip("Procly")
            .on_menu_event(|app, event| run(app, event.id().as_ref()))
            .build(app)?;
        Ok(Self {
            icon,
            state: Mutex::new(State {
                tracker,
                last: None,
                drawn: Drawn {
                    glyph: tracker.shown(),
                    model,
                    header,
                },
            }),
        })
    }

    fn held(&self) -> MutexGuard<'_, State> {
        // A panic while drawing leaves a state that the next answer draws over.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Takes an answer of the poller into account: the glyph, the lines of the menu and the time of the last state.
    pub(crate) fn show(&self, app: &AppHandle, presented: &Presented) {
        let mut state = self.held();
        let update = presented.update();
        let glyph = state.tracker.observe(&update.answer, presented.needs_you().is_empty());
        if let Some(snapshot) = tray_menu::snapshot(update, presented.needs_you()) {
            state.last = Some(snapshot);
        }
        let model = tray_menu::model(glyph, state.last.as_ref(), local);
        if let Err(error) = self.draw(&mut state.drawn, app, glyph, model) {
            eprintln!("procly: cannot update the menu bar icon: {error}");
        }
    }

    /// Changes only what changed: the image, the items when the shape of the menu changed, otherwise the header text
    /// in place, so that a menu held open is not replaced under the pointer.
    fn draw(&self, drawn: &mut Drawn, app: &AppHandle, glyph: Glyph, model: Model) -> tauri::Result<()> {
        if glyph != drawn.glyph {
            self.icon.set_icon(Some(image(glyph)?))?;
            // Setting an image resets how the system draws it.
            self.icon.set_icon_as_template(true)?;
            drawn.glyph = glyph;
        }
        if model.shape() != drawn.model.shape() {
            let (native, header) = menu(app, &model)?;
            self.icon.set_menu(Some(native))?;
            drawn.header = header;
        } else if model.header != drawn.model.header {
            drawn.header.set_text(&model.header)?;
        }
        drawn.model = model;
        Ok(())
    }
}
