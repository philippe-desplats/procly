//! The menu bar extra: a tray icon with a native menu, fed by the poller. Never a webview popover.
//!
//! What the menu says, which items it holds and which glyph shows are decided in `tray_menu.rs` and `glyph.rs`, which
//! are tested. This file only draws their result and runs what the developer chooses, so a closed window still lets
//! them see what needs them.

use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use tauri::image::Image;
use tauri::menu::{
    IconMenuItem, IconMenuItemBuilder, Menu, MenuItem, MenuItemBuilder, PredefinedMenuItem, SubmenuBuilder,
};
use tauri::tray::{TrayIcon, TrayIconBuilder};
use tauri::{AppHandle, Emitter as _, Manager as _, Wry};

use crate::clipboard;
use crate::glyph::{Glyph, GlyphTracker};
use crate::needs_you::{Place, Presented};
use crate::notifications::OPEN_FAILURE;
use crate::settings::OPEN_SETTINGS;
use crate::tray_menu::{self, Action, Entry, Item, LineGlyph, Model, Snapshot};
use crate::window::MainWindow;

/// The id of the one tray icon.
const TRAY_ID: &str = "procly";

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
    /// Where each line of "Needs you" on screen opens the window. Apart from `state`, so that a click read while the
    /// menu is being drawn waits for no drawing.
    places: Mutex<Vec<Place>>,
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

/// The native item of `item`.
fn native(app: &AppHandle, item: &Item) -> tauri::Result<MenuItem<Wry>> {
    MenuItemBuilder::with_id(&item.id, &item.label)
        .enabled(item.enabled)
        .build(app)
}

/// The native item of a line that leads with the state glyph `glyph`.
fn native_with(app: &AppHandle, item: &Item, glyph: LineGlyph) -> tauri::Result<IconMenuItem<Wry>> {
    IconMenuItemBuilder::with_id(&item.id, &item.label)
        .enabled(item.enabled)
        .icon(Image::from_bytes(glyph.png())?)
        .build(app)
}

/// The native menu of `model`, and its header. Which items it holds is decided by `tray_menu::layout`.
fn menu(app: &AppHandle, model: &Model) -> tauri::Result<(Menu<Wry>, MenuItem<Wry>)> {
    let layout = tray_menu::layout(model);
    let header = native(app, &layout.header)?;
    let menu = Menu::new(app)?;
    menu.append(&header)?;
    for entry in &layout.entries {
        match entry {
            Entry::Item(item) => match item.glyph {
                Some(glyph) => menu.append(&native_with(app, item, glyph)?)?,
                None => menu.append(&native(app, item)?)?,
            },
            Entry::Separator => menu.append(&PredefinedMenuItem::separator(app)?)?,
            Entry::Submenu { label, enabled, items } => {
                let mut submenu = SubmenuBuilder::new(app, *label).enabled(*enabled);
                for item in items {
                    submenu = submenu.item(&native(app, item)?);
                }
                menu.append(&submenu.build()?)?;
            }
        }
    }
    Ok((menu, header))
}

/// Runs what the developer chose in the menu. An id that is not an item of this menu is left to its owner.
fn run(app: &AppHandle, id: &str) {
    let Some(action) = tray_menu::action(id) else {
        return;
    };
    match action {
        Action::Open => MainWindow(app).open(),
        Action::OpenOn(index) => {
            let place = app.try_state::<Tray>().and_then(|tray| tray.place(index));
            MainWindow(app).open();
            // The window listens while it is hidden too, and shows the item once it is back.
            let _ = app.emit(OPEN_FAILURE, place);
        }
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
            places: Mutex::new(Vec::new()),
        })
    }

    fn held(&self) -> MutexGuard<'_, State> {
        // A panic while drawing leaves a state that the next answer draws over.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn places(&self) -> MutexGuard<'_, Vec<Place>> {
        // Nothing panics while holding it, and the list is replaced whole.
        self.places.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Where the line of "Needs you" at `index` on screen opens the window.
    fn place(&self, index: usize) -> Option<Place> {
        self.places().get(index).cloned()
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
        let places = model.places.clone();
        if let Err(error) = self.draw(&mut state.drawn, app, glyph, model) {
            eprintln!("procly: cannot update the menu bar icon: {error}");
        }
        *self.places() = places;
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
