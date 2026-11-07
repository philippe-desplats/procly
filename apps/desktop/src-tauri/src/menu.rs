//! The native menu bar of the app: Procly, File, Edit, View, Actions, Worktree, Window, Help, in that order.
//!
//! What the bar holds is a model first, [`BAR`], so that its order, its titles and its key equivalents are read by the
//! tests without a native menu. [`install`] draws it. The items of Actions and Worktree act on the selection of the
//! window, which tells Rust what it allows through [`set_menu_context`]: a native item cannot ask the window whether
//! it applies when the menu opens.

use std::sync::{Mutex, MutexGuard, PoisonError};

use serde::{Deserialize, Serialize};
use tauri::menu::{
    HELP_SUBMENU_ID, Menu, MenuEvent, MenuItem, MenuItemBuilder, PredefinedMenuItem, Submenu as NativeSubmenu,
    SubmenuBuilder, WINDOW_SUBMENU_ID,
};
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
    /// `⌘C` while the selection has a main value and no text is selected in the window: copies the main value, the
    /// port of a Service or the command that stops a Foreign process. It stands in the place of the standard Copy,
    /// which comes back as soon as text is selected, so that one item only ever holds `⌘C`.
    CopyMainValue,
    /// Opens the Find of the output of the selection and focuses it.
    FindInOutput,
    /// Shows the Worktree in the Finder.
    RevealInFinder,
    /// Copies the slug of the Worktree.
    CopySlug,
    /// Opens the documentation of Procly, [`HELP_URL`], in the browser.
    Help,
}

/// What Procly Help opens: the README of the repository, the address `Cargo.toml` declares.
pub(crate) const HELP_URL: &str = "https://github.com/philippe-desplats/procly#readme";

/// Every [`Choice`], so that an id is read back and every item is drawn.
const CHOICES: [Choice; 18] = [
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
    Choice::CopyMainValue,
    Choice::FindInOutput,
    Choice::RevealInFinder,
    Choice::CopySlug,
    Choice::Help,
];

impl Choice {
    /// Every item of the bar the app handles itself.
    #[cfg(test)]
    pub(crate) fn all() -> &'static [Self] {
        &CHOICES
    }

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
            Self::CopyMainValue => "copy-main-value",
            Self::FindInOutput => "find-in-output",
            Self::RevealInFinder => "reveal-in-finder",
            Self::CopySlug => "copy-slug",
            Self::Help => "help",
        }
    }

    /// The title of the item while the window sent `context`. Only Copy names what it copies.
    fn title_in(self, context: &MenuContext) -> String {
        match (self, context.main_value()) {
            (Self::CopyMainValue, Some((title, _))) => title,
            _ => self.title().to_owned(),
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
            Self::CopyMainValue => "Copy",
            Self::FindInOutput => "Find in Output…",
            Self::RevealInFinder => "Reveal in Finder",
            Self::CopySlug => "Copy Slug",
            Self::Help => "Procly Help",
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
            Self::CopyMainValue => Some("CmdOrCtrl+C"),
            Self::FindInOutput => Some("CmdOrCtrl+F"),
            Self::Orphans
            | Self::CopyLogsCommand
            | Self::Restart
            | Self::MarkSeen
            | Self::Release
            | Self::RevealInFinder
            | Self::CopySlug
            | Self::Help => None,
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
            Self::Settings | Self::Overview | Self::Doctor | Self::Orphans | Self::Refresh | Self::Help => true,
            Self::CopyLogsCommand => context.logs_command.is_some(),
            Self::Restart => context.restart,
            Self::Stop => context.stop,
            Self::MarkSeen => context.mark_seen,
            Self::OpenInEditor | Self::OpenInTerminal | Self::CopyPath | Self::Release | Self::RevealInFinder => {
                context.worktree.is_some()
            }
            Self::CopyMainValue => context.main_value().is_some(),
            Self::FindInOutput => context.find,
            Self::CopySlug => context.slug.is_some(),
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
    /// The Copy of Edit, which is the standard Copy or [`Choice::CopyMainValue`] after what the window sent.
    Copy,
}

/// One line of a menu as it shows for a context the window sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Line {
    Item(Choice),
    Standard(Standard),
    Separator,
}

impl Entry {
    /// The line this entry shows while the window sent `context`.
    fn resolve(self, context: &MenuContext) -> Line {
        match self {
            Self::Item(choice) => Line::Item(choice),
            Self::Standard(standard) => Line::Standard(standard),
            Self::Separator => Line::Separator,
            Self::Copy if context.main_value().is_some() => Line::Item(Choice::CopyMainValue),
            Self::Copy => Line::Standard(Standard::Copy),
        }
    }
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
            Entry::Copy,
            Entry::Standard(Standard::Paste),
            Entry::Standard(Standard::SelectAll),
            Entry::Separator,
            Entry::Item(Choice::FindInOutput),
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
            Entry::Item(Choice::RevealInFinder),
            Entry::Separator,
            Entry::Item(Choice::CopyPath),
            Entry::Item(Choice::CopySlug),
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
        entries: &[Entry::Item(Choice::Help)],
    },
];

/// What the selection of the window allows, which the window sends each time it changes. The default is what holds
/// while nothing is selected: every item of Actions and Worktree is disabled.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag is one thing the window says the selection allows, read by one item, never a state machine"
)]
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
    /// The slug of that Worktree, which Copy Slug copies.
    pub(crate) slug: Option<String>,
    /// The port the selection listens on, the main value of a Service.
    pub(crate) port: Option<u16>,
    /// The command that stops a Foreign process, its main value, which Procly never runs.
    pub(crate) stop_command: Option<String>,
    /// Whether text is selected in the window, in its output or in a field, which the standard Copy copies.
    pub(crate) text_selected: bool,
    /// Whether the selection has an output to find in, which a Foreign process does not.
    pub(crate) find: bool,
}

impl MenuContext {
    /// The title and the text of `⌘C`, none while text is selected in the window or the selection has no main value:
    /// the command that stops a Foreign process, otherwise the port of the selection.
    pub(crate) fn main_value(&self) -> Option<(String, String)> {
        if self.text_selected {
            return None;
        }
        if let Some(command) = &self.stop_command {
            return Some((format!("Copy {command}"), command.clone()));
        }
        self.port.map(|port| (format!("Copy Port {port}"), port.to_string()))
    }

    /// The text a copy item puts on the clipboard, none for another item or when there is nothing to copy.
    pub(crate) fn copied(&self, choice: Choice) -> Option<String> {
        match choice {
            Choice::CopyLogsCommand => self.logs_command.clone(),
            Choice::CopyPath => self.worktree.clone(),
            Choice::CopySlug => self.slug.clone(),
            Choice::CopyMainValue => self.main_value().map(|(_, text)| text),
            _ => None,
        }
    }
}

/// The Copy line of Edit: the standard Copy and the item that copies the main value, one of which is in the menu at a
/// time, at `position`.
struct CopyLine {
    edit: NativeSubmenu<Wry>,
    position: usize,
    standard: PredefinedMenuItem<Wry>,
    main_value: MenuItem<Wry>,
}

impl CopyLine {
    /// Puts in the menu the line `after` calls for in place of the one `before` did, and names it.
    fn show(&self, before: &MenuContext, after: &MenuContext) -> tauri::Result<()> {
        match (Entry::Copy.resolve(before), Entry::Copy.resolve(after)) {
            (Line::Standard(_), Line::Item(choice)) => {
                self.edit.remove(&self.standard)?;
                self.main_value.set_text(choice.title_in(after))?;
                self.edit.insert(&self.main_value, self.position)
            }
            (Line::Item(_), Line::Item(choice)) => self.main_value.set_text(choice.title_in(after)),
            (Line::Item(_), Line::Standard(_)) => {
                self.edit.remove(&self.main_value)?;
                self.edit.insert(&self.standard, self.position)
            }
            // The Copy line is never a separator, and a standard Copy stays as it is.
            _ => Ok(()),
        }
    }
}

/// The items of the bar that depend on the selection, and the last context the window sent.
pub(crate) struct MenuBar {
    items: Vec<(Choice, MenuItem<Wry>)>,
    copy: Option<CopyLine>,
    context: Mutex<MenuContext>,
}

impl MenuBar {
    /// The last context the window sent.
    pub(crate) fn context(&self) -> MutexGuard<'_, MenuContext> {
        self.context.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Enables the items `context` allows and disables the others, and gives `⌘C` to what it copies.
    fn apply(&self, context: MenuContext) {
        for (choice, item) in &self.items {
            if let Err(error) = item.set_enabled(choice.enabled(&context)) {
                eprintln!("procly: cannot update the menu item {}: {error}", choice.id());
            }
        }
        let mut held = self.context();
        if let Some(copy) = &self.copy
            && let Err(error) = copy.show(&held, &context)
        {
            eprintln!("procly: cannot update the Copy item: {error}");
        }
        *held = context;
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
    let mut copy = None;
    let menu = Menu::new(app)?;
    for submenu in &BAR {
        let mut builder = match submenu.id {
            Some(id) => SubmenuBuilder::with_id(app, id, submenu.title),
            None => SubmenuBuilder::new(app, submenu.title),
        };
        let mut copy_at = None;
        for (position, entry) in submenu.entries.iter().enumerate() {
            builder = match *entry {
                Entry::Item(choice) => {
                    let item = item(app, choice, &context)?;
                    let builder = builder.item(&item);
                    items.push((choice, item));
                    builder
                }
                Entry::Standard(standard) => standard_item(builder, standard),
                Entry::Separator => builder.separator(),
                Entry::Copy => {
                    // Nothing is selected yet, so the standard Copy holds `⌘C`.
                    let standard = PredefinedMenuItem::copy(app, None)?;
                    let builder = builder.item(&standard);
                    copy_at = Some((position, standard));
                    builder
                }
            };
        }
        let built = builder.build()?;
        menu.append(&built)?;
        if let Some((position, standard)) = copy_at {
            copy = Some(CopyLine {
                edit: built,
                position,
                standard,
                main_value: main_value_item(app)?,
            });
        }
    }
    app.set_menu(menu)?;
    Ok(MenuBar {
        items,
        copy,
        context: Mutex::new(context),
    })
}

/// The native item that copies the main value. It is enabled for good, since it is in the menu only while there is a
/// main value to copy, and [`CopyLine::show`] names it each time it shows.
fn main_value_item(app: &AppHandle) -> tauri::Result<MenuItem<Wry>> {
    let choice = Choice::CopyMainValue;
    let mut item = MenuItemBuilder::with_id(choice.id(), choice.title()).enabled(true);
    if let Some(accelerator) = choice.accelerator() {
        item = item.accelerator(accelerator);
    }
    item.build(app)
}

/// The native item of `choice`, as `context` enables it.
fn item(app: &AppHandle, choice: Choice, context: &MenuContext) -> tauri::Result<MenuItem<Wry>> {
    let mut item = MenuItemBuilder::with_id(choice.id(), choice.title_in(context)).enabled(choice.enabled(context));
    if let Some(accelerator) = choice.accelerator() {
        item = item.accelerator(accelerator);
    }
    item.build(app)
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

    /// The lines of the menu titled `title` while the window sent `context`, a custom item as its title and key
    /// equivalent, a standard one by its name.
    fn lines_in(title: &str, context: &MenuContext) -> Vec<String> {
        let submenu = BAR
            .iter()
            .find(|submenu| submenu.title == title)
            .expect("the menu is in the bar");
        submenu
            .entries
            .iter()
            .map(|entry| match entry.resolve(context) {
                Line::Item(choice) => match choice.accelerator() {
                    Some(accelerator) => format!("{} {accelerator}", choice.title_in(context)),
                    None => choice.title_in(context),
                },
                Line::Standard(standard) => format!("{standard:?}"),
                Line::Separator => "---".to_owned(),
            })
            .collect()
    }

    /// The lines of the menu titled `title` while nothing is selected.
    fn lines(title: &str) -> Vec<String> {
        lines_in(title, &MenuContext::default())
    }

    /// What the window sends while a Process service of `/p/work/acme/web` listening on 4101 is selected.
    fn service_on_4101() -> MenuContext {
        MenuContext {
            logs_command: Some("(cd /p/work/acme/web && procly logs api)".to_owned()),
            restart: true,
            stop: true,
            worktree: Some("/p/work/acme/web".to_owned()),
            slug: Some("acme_web".to_owned()),
            port: Some(4101),
            find: true,
            ..MenuContext::default()
        }
    }

    #[test]
    fn edit_copies_the_port_of_a_selected_service_with_command_c_and_names_it() {
        let edit = lines_in("Edit", &service_on_4101());

        assert_eq!(edit[4], "Copy Port 4101 CmdOrCtrl+C");
        assert!(!edit.contains(&"Copy".to_owned()), "one item holds ⌘C: {edit:?}");
        assert_eq!(service_on_4101().copied(Choice::CopyMainValue).as_deref(), Some("4101"));
    }

    #[test]
    fn edit_keeps_the_standard_copy_when_nothing_is_selected() {
        assert_eq!(
            lines("Edit"),
            [
                "Undo",
                "Redo",
                "---",
                "Cut",
                "Copy",
                "Paste",
                "SelectAll",
                "---",
                "Find in Output… CmdOrCtrl+F"
            ]
        );
        assert_eq!(MenuContext::default().copied(Choice::CopyMainValue), None);
    }

    #[test]
    fn edit_keeps_the_standard_copy_while_text_is_selected_in_the_window() {
        let selecting = MenuContext {
            text_selected: true,
            ..service_on_4101()
        };

        assert_eq!(lines_in("Edit", &selecting)[4], "Copy");
        assert_eq!(selecting.copied(Choice::CopyMainValue), None);
    }

    #[test]
    fn edit_keeps_the_standard_copy_for_a_selection_without_a_main_value() {
        let job = MenuContext {
            logs_command: Some("(cd /p/work/acme/web && procly logs j1)".to_owned()),
            find: true,
            ..MenuContext::default()
        };

        assert_eq!(lines_in("Edit", &job)[4], "Copy");
    }

    #[test]
    fn the_main_value_of_a_foreign_process_is_the_command_that_stops_it() {
        let foreign = MenuContext {
            port: Some(3000),
            stop_command: Some("kill 4812".to_owned()),
            worktree: Some("/p/work/acme/web".to_owned()),
            ..MenuContext::default()
        };

        assert_eq!(lines_in("Edit", &foreign)[4], "Copy kill 4812 CmdOrCtrl+C");
        assert_eq!(foreign.copied(Choice::CopyMainValue).as_deref(), Some("kill 4812"));
    }

    #[test]
    fn find_in_output_is_enabled_only_when_the_selection_has_an_output() {
        assert!(Choice::FindInOutput.enabled(&service_on_4101()));
        let foreign = MenuContext {
            stop_command: Some("kill 4812".to_owned()),
            ..MenuContext::default()
        };
        assert!(!Choice::FindInOutput.enabled(&foreign));
        assert!(!Choice::FindInOutput.enabled(&MenuContext::default()));
    }

    #[test]
    fn copy_slug_copies_the_slug_of_the_worktree() {
        assert_eq!(service_on_4101().copied(Choice::CopySlug).as_deref(), Some("acme_web"));
        assert!(Choice::CopySlug.enabled(&service_on_4101()));
        assert!(!Choice::CopySlug.enabled(&MenuContext::default()));
    }

    #[test]
    fn help_holds_procly_help() {
        assert_eq!(lines("Help"), ["Procly Help"]);
        assert!(Choice::Help.enabled(&MenuContext::default()));
        assert!(HELP_URL.starts_with("https://github.com/philippe-desplats/procly"));
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
    fn worktree_opens_reveals_copies_the_path_and_the_slug_and_releases_the_worktree() {
        assert_eq!(
            lines("Worktree"),
            [
                "Open in Editor CmdOrCtrl+E",
                "Open in Terminal CmdOrCtrl+T",
                "Reveal in Finder",
                "---",
                "Copy Path CmdOrCtrl+Alt+C",
                "Copy Slug",
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
                Entry::Copy => Some(Choice::CopyMainValue),
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

        assert_eq!(
            enabled,
            ["Settings…", "Overview", "Doctor", "Orphans", "Refresh", "Procly Help"]
        );
    }

    #[test]
    fn the_selection_enables_what_it_allows_and_nothing_else() {
        let disabled: Vec<_> = CHOICES
            .into_iter()
            .filter(|choice| !choice.enabled(&service_on_4101()))
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
            context.copied(Choice::CopyLogsCommand).as_deref(),
            Some("(cd /p/work/acme/web && procly logs api)")
        );
        assert_eq!(context.copied(Choice::CopyPath).as_deref(), Some("/p/work/acme/web"));
        assert_eq!(context.copied(Choice::Restart), None);
        assert_eq!(MenuContext::default().copied(Choice::CopyPath), None);
    }
}
