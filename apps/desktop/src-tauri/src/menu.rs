//! The native menu bar of the app: Procly, File, Edit, View, Actions, Worktree, Window, Help, in that order.
//!
//! What the bar holds is a model first, [`BAR`], so that its order, its titles and its key equivalents are read by the
//! tests without a native menu. [`install`] draws it. The items of Actions and Worktree act on the selection of the
//! window, which tells Rust what it allows through [`set_menu_context`]: a native item cannot ask the window whether
//! it applies when the menu opens.

use std::sync::{Mutex, MutexGuard, PoisonError};

use serde::{Deserialize, Serialize};
use tauri::menu::{HELP_SUBMENU_ID, Menu, MenuEvent, MenuItem, MenuItemBuilder, SubmenuBuilder, WINDOW_SUBMENU_ID};
use tauri::{AppHandle, State, Wry};

/// An item of the menu bar the app handles itself, as opposed to the standard items macOS handles.
///
/// The window receives a choice it carries out as its id, and reads it through the type ts-rs exports from this enum
/// (ADR 0015), so the ids cannot drift between the two.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
#[cfg_attr(test, derive(ts_rs::TS), ts(export, rename = "MenuChoice"))]
pub(crate) enum Choice {
    /// Opens Settings.
    Settings,
    /// Shows the Overview.
    Overview,
    /// Shows Doctor.
    Doctor,
    /// Shows the Orphans.
    Orphans,
    /// Asks the poller for an answer at once. It never restarts anything.
    Refresh,
    /// Copies the `procly logs` command of the selection.
    CopyLogsCommand,
    /// Restarts the selection, as the inspector does.
    Restart,
    /// Stops the selection, as the inspector does.
    Stop,
    /// Marks the selected ended failure as Seen.
    MarkSeen,
    /// Opens the Worktree in the editor of the settings.
    OpenInEditor,
    /// Opens the Worktree in the terminal of the settings.
    OpenInTerminal,
    /// Copies the path of the Worktree.
    CopyPath,
    /// Opens the release sheet of the Worktree.
    Release,
}

/// Every [`Choice`], so that an id is read back and every item is drawn.
const CHOICES: [Choice; 13] = [
    Choice::Settings,
    Choice::Overview,
    Choice::Doctor,
    Choice::Orphans,
    Choice::Refresh,
    Choice::CopyLogsCommand,
    Choice::Restart,
    Choice::Stop,
    Choice::MarkSeen,
    Choice::OpenInEditor,
    Choice::OpenInTerminal,
    Choice::CopyPath,
    Choice::Release,
];

impl Choice {
    /// The id of the item, which is also what the window receives for the choices it carries out.
    pub(crate) fn id(self) -> &'static str {
        match self {
            Self::Settings => "settings",
            Self::Overview => "overview",
            Self::Doctor => "doctor",
            Self::Orphans => "orphans",
            Self::Refresh => "refresh",
            Self::CopyLogsCommand => "copy-logs-command",
            Self::Restart => "restart",
            Self::Stop => "stop",
            Self::MarkSeen => "mark-seen",
            Self::OpenInEditor => "open-in-editor",
            Self::OpenInTerminal => "open-in-terminal",
            Self::CopyPath => "copy-path",
            Self::Release => "release",
        }
    }

    /// The title of the item, on one line.
    fn title(self) -> &'static str {
        match self {
            Self::Settings => "Settings…",
            Self::Overview => "Overview",
            Self::Doctor => "Doctor",
            Self::Orphans => "Orphans",
            Self::Refresh => "Refresh",
            Self::CopyLogsCommand => "Copy Logs Command",
            Self::Restart => "Restart",
            Self::Stop => "Stop",
            Self::MarkSeen => "Mark as Seen",
            Self::OpenInEditor => "Open in Editor",
            Self::OpenInTerminal => "Open in Terminal",
            Self::CopyPath => "Copy Path",
            Self::Release => "Release…",
        }
    }

    /// The key equivalent of the item. Each has one character, and none is a digit from 1 to 9, which no Worktree
    /// gets. Restart has none, since `⌘R` is Refresh.
    fn accelerator(self) -> Option<&'static str> {
        match self {
            Self::Settings => Some("CmdOrCtrl+,"),
            Self::Overview => Some("CmdOrCtrl+0"),
            Self::Doctor => Some("CmdOrCtrl+Shift+D"),
            Self::Refresh => Some("CmdOrCtrl+R"),
            Self::Stop => Some("CmdOrCtrl+."),
            Self::OpenInEditor => Some("CmdOrCtrl+E"),
            Self::OpenInTerminal => Some("CmdOrCtrl+T"),
            Self::CopyPath => Some("CmdOrCtrl+Alt+C"),
            Self::Orphans | Self::CopyLogsCommand | Self::Restart | Self::MarkSeen | Self::Release => None,
        }
    }

    /// The item an event of the menu is the choice of, none for a standard item.
    pub(crate) fn of(event: &MenuEvent) -> Option<Self> {
        Self::from_id(event.id().as_ref())
    }

    fn from_id(id: &str) -> Option<Self> {
        CHOICES.into_iter().find(|choice| choice.id() == id)
    }

    /// Whether the item can be chosen with what the window has selected. The items of the Procly and View menus always
    /// can.
    fn enabled(self, context: &MenuContext) -> bool {
        match self {
            Self::Settings | Self::Overview | Self::Doctor | Self::Orphans | Self::Refresh => true,
            Self::CopyLogsCommand => context.logs_command.is_some(),
            Self::Restart => context.restart,
            Self::Stop => context.stop,
            Self::MarkSeen => context.mark_seen,
            Self::OpenInEditor | Self::OpenInTerminal | Self::CopyPath | Self::Release => context.worktree.is_some(),
        }
    }
}

/// A standard item, which macOS carries out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Standard {
    About,
    Hide,
    HideOthers,
    ShowAll,
    Quit,
    /// `⌘W`. It closes the window, which hides it: nothing stops.
    CloseWindow,
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    SelectAll,
    /// `⌘M`.
    Minimize,
    Zoom,
    BringAllToFront,
}

/// One line of a menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Entry {
    Item(Choice),
    Standard(Standard),
    Separator,
}

/// One menu of the bar: its title, the id Tauri knows it by when it gives it a role in macOS, and its lines.
struct Submenu {
    title: &'static str,
    id: Option<&'static str>,
    entries: &'static [Entry],
}

/// The menu bar. On macOS the first menu is the app menu, named after the app. Tauri makes the Window menu the one that
/// lists the windows and the Help menu the one with the search field of the system, from their ids.
const BAR: [Submenu; 8] = [
    Submenu {
        title: "Procly",
        id: None,
        entries: &[
            Entry::Standard(Standard::About),
            Entry::Separator,
            Entry::Item(Choice::Settings),
            Entry::Separator,
            Entry::Standard(Standard::Hide),
            Entry::Standard(Standard::HideOthers),
            Entry::Standard(Standard::ShowAll),
            Entry::Separator,
            Entry::Standard(Standard::Quit),
        ],
    },
    Submenu {
        title: "File",
        id: None,
        entries: &[Entry::Standard(Standard::CloseWindow)],
    },
    Submenu {
        title: "Edit",
        id: None,
        entries: &[
            Entry::Standard(Standard::Undo),
            Entry::Standard(Standard::Redo),
            Entry::Separator,
            Entry::Standard(Standard::Cut),
            Entry::Standard(Standard::Copy),
            Entry::Standard(Standard::Paste),
            Entry::Standard(Standard::SelectAll),
        ],
    },
    Submenu {
        title: "View",
        id: None,
        entries: &[
            Entry::Item(Choice::Overview),
            Entry::Item(Choice::Doctor),
            Entry::Item(Choice::Orphans),
            Entry::Separator,
            Entry::Item(Choice::Refresh),
        ],
    },
    Submenu {
        title: "Actions",
        id: None,
        entries: &[
            Entry::Item(Choice::CopyLogsCommand),
            Entry::Item(Choice::Restart),
            Entry::Separator,
            Entry::Item(Choice::Stop),
            Entry::Item(Choice::MarkSeen),
        ],
    },
    Submenu {
        title: "Worktree",
        id: None,
        entries: &[
            Entry::Item(Choice::OpenInEditor),
            Entry::Item(Choice::OpenInTerminal),
            Entry::Separator,
            Entry::Item(Choice::CopyPath),
            Entry::Separator,
            Entry::Item(Choice::Release),
        ],
    },
    Submenu {
        title: "Window",
        id: Some(WINDOW_SUBMENU_ID),
        entries: &[
            Entry::Standard(Standard::Minimize),
            Entry::Standard(Standard::Zoom),
            Entry::Separator,
            Entry::Standard(Standard::BringAllToFront),
        ],
    },
    Submenu {
        title: "Help",
        id: Some(HELP_SUBMENU_ID),
        entries: &[],
    },
];

/// What the selection of the window allows, which the window sends each time it changes. The default is what holds
/// while nothing is selected: every item of Actions and Worktree is disabled.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub(crate) struct MenuContext {
    /// The `procly logs` command of the selection, which Copy Logs Command copies.
    pub(crate) logs_command: Option<String>,
    /// Whether the inspector offers Restart for the selection.
    pub(crate) restart: bool,
    /// Whether the selection is a Service or an Isolated Compose service that runs.
    pub(crate) stop: bool,
    /// Whether the selection is an ended failure that is not Seen.
    pub(crate) mark_seen: bool,
    /// The path of the Worktree the inspector acts on, which the Worktree menu acts on too.
    pub(crate) worktree: Option<String>,
}

impl MenuContext {
    /// The text a copy item puts on the clipboard, none for another item or when there is nothing to copy.
    pub(crate) fn copied(&self, choice: Choice) -> Option<&str> {
        match choice {
            Choice::CopyLogsCommand => self.logs_command.as_deref(),
            Choice::CopyPath => self.worktree.as_deref(),
            _ => None,
        }
    }
}

/// The items of the bar that depend on the selection, and the last context the window sent.
pub(crate) struct MenuBar {
    items: Vec<(Choice, MenuItem<Wry>)>,
    context: Mutex<MenuContext>,
}

impl MenuBar {
    /// The last context the window sent.
    pub(crate) fn context(&self) -> MutexGuard<'_, MenuContext> {
        self.context.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Enables the items `context` allows and disables the others.
    fn apply(&self, context: MenuContext) {
        for (choice, item) in &self.items {
            if let Err(error) = item.set_enabled(choice.enabled(&context)) {
                eprintln!("procly: cannot update the menu item {}: {error}", choice.id());
            }
        }
        *self.context() = context;
    }
}

/// Draws [`BAR`] and makes it the menu bar of the app.
///
/// # Errors
///
/// When Tauri cannot build a native menu item or set the menu.
pub(crate) fn install(app: &AppHandle) -> tauri::Result<MenuBar> {
    let context = MenuContext::default();
    let mut items = Vec::new();
    let menu = Menu::new(app)?;
    for submenu in &BAR {
        let mut builder = match submenu.id {
            Some(id) => SubmenuBuilder::with_id(app, id, submenu.title),
            None => SubmenuBuilder::new(app, submenu.title),
        };
        for entry in submenu.entries {
            builder = match *entry {
                Entry::Item(choice) => {
                    let mut item =
                        MenuItemBuilder::with_id(choice.id(), choice.title()).enabled(choice.enabled(&context));
                    if let Some(accelerator) = choice.accelerator() {
                        item = item.accelerator(accelerator);
                    }
                    let item = item.build(app)?;
                    let builder = builder.item(&item);
                    items.push((choice, item));
                    builder
                }
                Entry::Standard(standard) => standard_item(builder, standard),
                Entry::Separator => builder.separator(),
            };
        }
        menu.append(&builder.build()?)?;
    }
    app.set_menu(menu)?;
    Ok(MenuBar {
        items,
        context: Mutex::new(context),
    })
}

/// Adds the standard item `standard` to `builder`.
fn standard_item(
    builder: SubmenuBuilder<'_, Wry, AppHandle>,
    standard: Standard,
) -> SubmenuBuilder<'_, Wry, AppHandle> {
    match standard {
        Standard::About => builder.about(None),
        Standard::Hide => builder.hide(),
        Standard::HideOthers => builder.hide_others(),
        Standard::ShowAll => builder.show_all(),
        Standard::Quit => builder.quit(),
        Standard::CloseWindow => builder.close_window(),
        Standard::Undo => builder.undo(),
        Standard::Redo => builder.redo(),
        Standard::Cut => builder.cut(),
        Standard::Copy => builder.copy(),
        Standard::Paste => builder.paste(),
        Standard::SelectAll => builder.select_all(),
        Standard::Minimize => builder.minimize(),
        Standard::Zoom => builder.maximize(),
        Standard::BringAllToFront => builder.bring_all_to_front(),
    }
}

/// Enables and disables the items of Actions and Worktree from what the selection of the window allows.
#[tauri::command]
#[expect(
    clippy::needless_pass_by_value,
    reason = "Tauri hands managed state to a command as a `State`"
)]
pub(crate) fn set_menu_context(context: MenuContext, bar: State<'_, MenuBar>) {
    bar.apply(context);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The lines of the menu titled `title`, a custom item as its title and key equivalent, a standard one by its name.
    fn lines(title: &str) -> Vec<String> {
        let submenu = BAR
            .iter()
            .find(|submenu| submenu.title == title)
            .expect("the menu is in the bar");
        submenu
            .entries
            .iter()
            .map(|entry| match entry {
                Entry::Item(choice) => match choice.accelerator() {
                    Some(accelerator) => format!("{} {accelerator}", choice.title()),
                    None => choice.title().to_owned(),
                },
                Entry::Standard(standard) => format!("{standard:?}"),
                Entry::Separator => "---".to_owned(),
            })
            .collect()
    }

    #[test]
    fn the_bar_holds_the_menus_of_the_shape_in_order() {
        let titles: Vec<_> = BAR.iter().map(|submenu| submenu.title).collect();

        assert_eq!(
            titles,
            [
                "Procly", "File", "Edit", "View", "Actions", "Worktree", "Window", "Help"
            ]
        );
    }

    #[test]
    fn the_procly_menu_holds_about_settings_hide_and_quit_and_no_refresh() {
        assert_eq!(
            lines("Procly"),
            [
                "About",
                "---",
                "Settings… CmdOrCtrl+,",
                "---",
                "Hide",
                "HideOthers",
                "ShowAll",
                "---",
                "Quit"
            ]
        );
    }

    #[test]
    fn file_closes_the_window_and_window_minimizes_it() {
        assert_eq!(lines("File"), ["CloseWindow"]);
        assert_eq!(lines("Window"), ["Minimize", "Zoom", "---", "BringAllToFront"]);
    }

    #[test]
    fn view_goes_to_the_overview_doctor_and_the_orphans_and_refreshes() {
        assert_eq!(
            lines("View"),
            [
                "Overview CmdOrCtrl+0",
                "Doctor CmdOrCtrl+Shift+D",
                "Orphans",
                "---",
                "Refresh CmdOrCtrl+R"
            ]
        );
    }

    #[test]
    fn actions_offers_restart_with_no_key_equivalent_and_stop_with_command_period() {
        assert_eq!(
            lines("Actions"),
            [
                "Copy Logs Command",
                "Restart",
                "---",
                "Stop CmdOrCtrl+.",
                "Mark as Seen"
            ]
        );
    }

    #[test]
    fn worktree_opens_copies_the_path_and_releases_the_worktree() {
        assert_eq!(
            lines("Worktree"),
            [
                "Open in Editor CmdOrCtrl+E",
                "Open in Terminal CmdOrCtrl+T",
                "---",
                "Copy Path CmdOrCtrl+Alt+C",
                "---",
                "Release…"
            ]
        );
    }

    #[test]
    fn the_help_and_window_menus_carry_the_ids_that_give_them_their_role_in_macos() {
        let ids: Vec<_> = BAR.iter().map(|submenu| (submenu.title, submenu.id)).collect();

        assert!(ids.contains(&("Window", Some(WINDOW_SUBMENU_ID))));
        assert!(ids.contains(&("Help", Some(HELP_SUBMENU_ID))));
    }

    #[test]
    fn every_key_equivalent_is_one_character_and_none_is_a_digit_from_1_to_9() {
        for choice in CHOICES {
            let Some(accelerator) = choice.accelerator() else {
                continue;
            };
            let key = accelerator.rsplit('+').next().expect("a key");
            assert_eq!(key.chars().count(), 1, "{accelerator}");
            assert!(!('1'..='9').any(|digit| key.starts_with(digit)), "{accelerator}");
        }
    }

    #[test]
    fn every_choice_is_drawn_once_and_read_back_from_its_id() {
        let drawn: Vec<_> = BAR
            .iter()
            .flat_map(|submenu| submenu.entries)
            .filter_map(|entry| match entry {
                Entry::Item(choice) => Some(*choice),
                Entry::Standard(_) | Entry::Separator => None,
            })
            .collect();

        assert_eq!(drawn.len(), CHOICES.len());
        for choice in CHOICES {
            assert!(drawn.contains(&choice), "{choice:?}");
            assert_eq!(Choice::from_id(choice.id()), Some(choice));
        }
        assert_eq!(Choice::from_id("about"), None);
    }

    #[test]
    fn every_choice_reaches_the_window_as_the_id_of_its_item() {
        // The window reads the choice through the type ts-rs exports from this enum, so its ids are the ones of Rust.
        for choice in CHOICES {
            assert_eq!(
                serde_json::to_value(choice).expect("a choice serializes"),
                serde_json::Value::from(choice.id()),
                "{choice:?}"
            );
        }
    }

    #[test]
    fn with_nothing_selected_only_the_procly_and_view_items_are_enabled() {
        let enabled: Vec<_> = CHOICES
            .into_iter()
            .filter(|choice| choice.enabled(&MenuContext::default()))
            .map(Choice::title)
            .collect();

        assert_eq!(enabled, ["Settings…", "Overview", "Doctor", "Orphans", "Refresh"]);
    }

    #[test]
    fn the_selection_enables_what_it_allows_and_nothing_else() {
        let running = MenuContext {
            logs_command: Some("(cd /p/work/acme/web && procly logs api)".to_owned()),
            restart: true,
            stop: true,
            mark_seen: false,
            worktree: Some("/p/work/acme/web".to_owned()),
        };
        let disabled: Vec<_> = CHOICES
            .into_iter()
            .filter(|choice| !choice.enabled(&running))
            .map(Choice::title)
            .collect();

        assert_eq!(disabled, ["Mark as Seen"]);
    }

    #[test]
    fn copy_logs_command_and_copy_path_copy_what_the_window_sent() {
        let context = MenuContext {
            logs_command: Some("(cd /p/work/acme/web && procly logs api)".to_owned()),
            worktree: Some("/p/work/acme/web".to_owned()),
            ..MenuContext::default()
        };

        assert_eq!(
            context.copied(Choice::CopyLogsCommand),
            Some("(cd /p/work/acme/web && procly logs api)")
        );
        assert_eq!(context.copied(Choice::CopyPath), Some("/p/work/acme/web"));
        assert_eq!(context.copied(Choice::Restart), None);
        assert_eq!(MenuContext::default().copied(Choice::CopyPath), None);
    }
}
