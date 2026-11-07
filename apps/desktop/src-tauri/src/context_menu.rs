//! The context menus of the window, on a row and on a Worktree of the sidebar: native macOS menus shown at the
//! pointer, as the menu bar is, so that the window needs no permission of its own to show one.
//!
//! The window knows what the row it was right-clicked on offers, so it sends the entries. Rust draws them, copies the
//! text of a copy entry itself, as the copy items of the menu bar do, and tells the window the index of any other
//! entry chosen, which the window carries out.

use std::sync::{Mutex, MutexGuard, PoisonError};

use serde::Deserialize;
use tauri::menu::{Menu, MenuItemBuilder, PredefinedMenuItem};
use tauri::{State, Window};

/// The event that tells the window which entry of the context menu it showed last was chosen, by its index.
pub(crate) const CONTEXT_CHOICE: &str = "context-choice";

/// What the id of every entry starts with, which no item of the menu bar nor of the menu bar menu does.
const PREFIX: &str = "context-";

/// One line of a context menu, as the window sends it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub(crate) enum ContextEntry {
    /// An item that puts `text` on the clipboard, which Rust does.
    Copy {
        title: String,
        text: String,
    },
    /// An item the window carries out once it is told its index.
    Act {
        title: String,
    },
    Separator,
}

/// What choosing an entry of the last context menu does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Picked<'a> {
    /// Rust copies this text.
    Copy(&'a str),
    /// The window carries out the entry at this index.
    Act(usize),
}

/// The id of the entry at `index`.
fn id(index: usize) -> String {
    format!("{PREFIX}{index}")
}

/// What choosing the item with `id` does in a menu of `entries`, nothing for an id that is not one of its items.
pub(crate) fn picked<'a>(entries: &'a [ContextEntry], id: &str) -> Option<Picked<'a>> {
    let index: usize = id.strip_prefix(PREFIX)?.parse().ok()?;
    match entries.get(index)? {
        ContextEntry::Copy { text, .. } => Some(Picked::Copy(text)),
        ContextEntry::Act { .. } => Some(Picked::Act(index)),
        ContextEntry::Separator => None,
    }
}

/// The entries of the context menu shown last, which its choices are read against.
#[derive(Debug, Default)]
pub(crate) struct ContextMenus(Mutex<Vec<ContextEntry>>);

impl ContextMenus {
    /// The entries of the context menu shown last.
    pub(crate) fn shown(&self) -> MutexGuard<'_, Vec<ContextEntry>> {
        // The list is replaced whole, a panic leaves the previous one.
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The native menu of `entries`.
fn native(window: &Window, entries: &[ContextEntry]) -> tauri::Result<Menu<tauri::Wry>> {
    let menu = Menu::new(window)?;
    for (index, entry) in entries.iter().enumerate() {
        match entry {
            ContextEntry::Copy { title, .. } | ContextEntry::Act { title } => {
                menu.append(&MenuItemBuilder::with_id(id(index), title).build(window)?)?;
            }
            ContextEntry::Separator => menu.append(&PredefinedMenuItem::separator(window)?)?,
        }
    }
    Ok(menu)
}

/// Shows `entries` as a native context menu at the pointer. A menu that cannot be drawn shows nothing: the same
/// actions stay in the menu bar and the inspector.
#[tauri::command]
#[expect(
    clippy::needless_pass_by_value,
    reason = "Tauri hands the window and managed state to a command by value"
)]
pub(crate) fn show_context_menu(entries: Vec<ContextEntry>, window: Window, menus: State<'_, ContextMenus>) {
    let menu = match native(&window, &entries) {
        Ok(menu) => menu,
        Err(error) => {
            eprintln!("procly: cannot build the context menu: {error}");
            return;
        }
    };
    *menus.shown() = entries;
    if let Err(error) = window.popup_menu(&menu) {
        eprintln!("procly: cannot show the context menu: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::menu::Choice;

    fn entries() -> Vec<ContextEntry> {
        vec![
            ContextEntry::Copy {
                title: "Copy Port 4101".to_owned(),
                text: "4101".to_owned(),
            },
            ContextEntry::Separator,
            ContextEntry::Act {
                title: "Mark as Unseen".to_owned(),
            },
        ]
    }

    #[test]
    fn a_copy_entry_copies_its_text_and_another_entry_goes_back_to_the_window_by_its_index() {
        let entries = entries();

        assert_eq!(picked(&entries, &id(0)), Some(Picked::Copy("4101")));
        assert_eq!(picked(&entries, &id(2)), Some(Picked::Act(2)));
    }

    #[test]
    fn a_separator_an_index_past_the_end_and_a_foreign_id_do_nothing() {
        let entries = entries();

        assert_eq!(picked(&entries, &id(1)), None);
        assert_eq!(picked(&entries, &id(3)), None);
        assert_eq!(picked(&entries, "context-first"), None);
        assert_eq!(picked(&entries, "tray-needs-you-0"), None);
        assert_eq!(picked(&entries, "copy-path"), None);
    }

    #[test]
    fn no_item_of_the_menu_bar_nor_of_the_menu_bar_menu_is_taken_for_an_entry() {
        assert!(Choice::all().iter().all(|choice| !choice.id().starts_with(PREFIX)));
        assert!(!crate::tray_menu::OPEN_ID.starts_with(PREFIX));
        assert!(!crate::tray_menu::needs_you_id(0).starts_with(PREFIX));
        assert!(!crate::tray_menu::copy_port_id(4101).starts_with(PREFIX));
    }

    #[test]
    fn the_window_sends_entries_tagged_by_their_kind() {
        let sent = serde_json::json!([
            { "kind": "copy", "title": "Copy Port 4101", "text": "4101" },
            { "kind": "separator" },
            { "kind": "act", "title": "Mark as Unseen" },
        ]);

        let read: Vec<ContextEntry> = serde_json::from_value(sent).expect("the entries deserialize");

        assert_eq!(read, entries());
    }
}
