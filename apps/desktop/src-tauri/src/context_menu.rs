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

use crate::menu::Choice;

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
    /// An item the window carries out once it is told its index. `accelerator` names the item of the menu bar whose
    /// key equivalent this line shows, so that the 2 are one definition and cannot drift. The context menu only shows
    /// the key: the menu bar keeps acting on it, on the selection it acts on today.
    Act {
        title: String,
        #[serde(default)]
        #[cfg_attr(test, ts(optional))]
        accelerator: Option<Choice>,
    },
    Separator,
}

/// What choosing an entry of the last context menu does.
/// It holds its text, so that the entries can be let go of before copying.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Picked {
    /// Rust copies this text.
    Copy(String),
    /// The window carries out the entry at this index.
    Act(usize),
}

/// The id of the entry at `index`.
fn id(index: usize) -> String {
    format!("{PREFIX}{index}")
}

/// What choosing the item with `id` does in a menu of `entries`, nothing for an id that is not one of its items.
pub(crate) fn picked(entries: &[ContextEntry], id: &str) -> Option<Picked> {
    let index: usize = id.strip_prefix(PREFIX)?.parse().ok()?;
    match entries.get(index)? {
        ContextEntry::Copy { text, .. } => Some(Picked::Copy(text.clone())),
        ContextEntry::Act { .. } => Some(Picked::Act(index)),
        ContextEntry::Separator => None,
    }
}

/// The key equivalent an entry shows, the one of the item of the menu bar it names, none for an entry that names none.
fn key_of(entry: &ContextEntry) -> Option<&'static str> {
    match entry {
        ContextEntry::Act {
            accelerator: Some(choice),
            ..
        } => choice.accelerator(),
        ContextEntry::Act { .. } | ContextEntry::Copy { .. } | ContextEntry::Separator => None,
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
            ContextEntry::Copy { title, .. } | ContextEntry::Act { title, .. } => {
                let mut item = MenuItemBuilder::with_id(id(index), title);
                if let Some(key) = key_of(entry) {
                    item = item.accelerator(key);
                }
                menu.append(&item.build(window)?)?;
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

    fn entries() -> Vec<ContextEntry> {
        vec![
            ContextEntry::Copy {
                title: "Copy Port 4101".to_owned(),
                text: "4101".to_owned(),
            },
            ContextEntry::Separator,
            ContextEntry::Act {
                title: "Mark as Unseen".to_owned(),
                accelerator: None,
            },
        ]
    }

    #[test]
    fn an_act_entry_shows_the_key_of_the_item_of_the_menu_bar_it_names_and_the_same_key_as_the_bar() {
        let stop: ContextEntry =
            serde_json::from_str(r#"{"kind":"act","title":"Stop","accelerator":"stop"}"#).expect("a Stop entry");

        assert_eq!(
            stop,
            ContextEntry::Act {
                title: "Stop".to_owned(),
                accelerator: Some(Choice::Stop),
            }
        );
        // The one definition of the key is the menu bar's: the entry reads it, it does not restate it.
        assert_eq!(key_of(&stop), Choice::Stop.accelerator());
        assert_eq!(key_of(&stop), Some("CmdOrCtrl+."));
    }

    #[test]
    fn an_entry_that_names_no_item_of_the_menu_bar_shows_no_key() {
        let plain: ContextEntry =
            serde_json::from_str(r#"{"kind":"act","title":"Mark as Seen"}"#).expect("an entry with no accelerator");

        assert_eq!(key_of(&plain), None);
        // An item of the menu bar with no key of its own gives none to the entry that names it.
        let release = ContextEntry::Act {
            title: "Release Worktree…".to_owned(),
            accelerator: Some(Choice::Release),
        };
        assert_eq!(key_of(&release), None);
        assert_eq!(key_of(&ContextEntry::Separator), None);
        assert_eq!(
            key_of(&ContextEntry::Copy {
                title: "Copy Port 1".to_owned(),
                text: "1".to_owned()
            }),
            None
        );
    }

    #[test]
    fn a_copy_entry_copies_its_text_and_another_entry_goes_back_to_the_window_by_its_index() {
        let entries = entries();

        assert_eq!(picked(&entries, &id(0)), Some(Picked::Copy("4101".to_owned())));
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
