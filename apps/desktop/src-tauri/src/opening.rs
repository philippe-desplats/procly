//! Opening a Worktree in the editor, the terminal or the Finder, from the app the settings name.
//!
//! Rust opens, so that the window holds no `opener` permission and cannot open anything but a Worktree through the
//! commands of the app (`docs/rules/tauri.md`).

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize, Serializer};
use tauri::State;

use crate::settings::{Settings, SettingsStore};

/// What the window asks to open a Worktree with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OpenWith {
    Editor,
    Terminal,
}

/// The system, as far as opening goes.
pub(crate) trait Launcher {
    /// Opens `path` with the app `app` names, as `open -a` does.
    fn open_with(&self, path: &Path, app: &str) -> io::Result<()>;

    /// Shows `path` in the Finder.
    fn reveal(&self, path: &Path) -> io::Result<()>;
}

/// The `opener` plugin, which needs no plugin set up for what Rust calls of it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Opener;

impl Launcher for Opener {
    fn open_with(&self, path: &Path, app: &str) -> io::Result<()> {
        tauri_plugin_opener::open_path(path, Some(app)).map_err(io::Error::other)
    }

    fn reveal(&self, path: &Path) -> io::Result<()> {
        tauri_plugin_opener::reveal_item_in_dir(path).map_err(io::Error::other)
    }
}

/// A Worktree could not be opened. The window opens Settings on the editor for `NoEditor` and shows the message of
/// every other one.
#[derive(Debug, thiserror::Error)]
pub(crate) enum OpenError {
    #[error("no editor is set, choose one in Settings")]
    NoEditor,
    #[error("{} is not a folder any more", path.display())]
    Missing { path: PathBuf },
    #[error("cannot open {} with {app}: {source}", path.display())]
    Launch {
        app: String,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cannot reveal {} in the Finder: {source}", path.display())]
    Reveal {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cannot open the Worktree: {0}")]
    Task(String),
}

impl OpenError {
    /// The thread that opens stopped before it could.
    pub(crate) fn task(error: &tauri::Error) -> Self {
        Self::Task(error.to_string())
    }

    fn kind(&self) -> &'static str {
        match self {
            Self::NoEditor => "no_editor",
            Self::Missing { .. } => "missing",
            Self::Launch { .. } => "launch",
            Self::Reveal { .. } => "reveal",
            Self::Task(_) => "task",
        }
    }
}

impl Serialize for OpenError {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Failure {
            kind: &'static str,
            message: String,
        }
        Failure {
            kind: self.kind(),
            message: self.to_string(),
        }
        .serialize(serializer)
    }
}

fn check_folder(path: &Path) -> Result<(), OpenError> {
    if path.is_dir() {
        Ok(())
    } else {
        Err(OpenError::Missing {
            path: path.to_path_buf(),
        })
    }
}

/// Opens the Worktree at `path` with the editor or the terminal the settings name.
///
/// # Errors
///
/// When there is no editor set, the folder is gone, or the app does not open it.
pub(crate) fn open(
    launcher: &impl Launcher,
    settings: &Settings,
    path: &Path,
    with: OpenWith,
) -> Result<(), OpenError> {
    let app = match with {
        OpenWith::Editor => settings.editor.as_deref().ok_or(OpenError::NoEditor)?,
        OpenWith::Terminal => settings.terminal.as_str(),
    };
    check_folder(path)?;
    launcher.open_with(path, app).map_err(|source| OpenError::Launch {
        app: app.to_owned(),
        path: path.to_path_buf(),
        source,
    })
}

/// Shows the Worktree at `path` in the Finder.
///
/// # Errors
///
/// When the folder is gone, or the Finder does not show it.
pub(crate) fn reveal(launcher: &impl Launcher, path: &Path) -> Result<(), OpenError> {
    check_folder(path)?;
    launcher.reveal(path).map_err(|source| OpenError::Reveal {
        path: path.to_path_buf(),
        source,
    })
}

/// Opens the Worktree at `path` in the editor or the terminal of the settings.
///
/// # Errors
///
/// An [`OpenError`], which the window shows. `NoEditor` sends it to Settings instead.
#[tauri::command]
pub(crate) async fn open_worktree(
    path: PathBuf,
    with: OpenWith,
    settings: State<'_, SettingsStore>,
) -> Result<(), OpenError> {
    let settings = settings.get();
    // The app is asked to open and waited for, which blocks.
    tauri::async_runtime::spawn_blocking(move || open(&Opener, &settings, &path, with))
        .await
        .map_err(|error| OpenError::task(&error))?
}

/// Shows the Worktree at `path` in the Finder.
///
/// # Errors
///
/// An [`OpenError`], which the window shows.
#[tauri::command]
pub(crate) async fn reveal_worktree(path: PathBuf) -> Result<(), OpenError> {
    tauri::async_runtime::spawn_blocking(move || reveal(&Opener, &path))
        .await
        .map_err(|error| OpenError::task(&error))?
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::io;
    use std::path::{Path, PathBuf};

    use super::{Launcher, OpenError, OpenWith, open, reveal};
    use crate::settings::Settings;

    /// Stands for the system: what it was asked to launch is kept, and it can refuse.
    #[derive(Default)]
    struct Fake {
        launched: RefCell<Vec<(PathBuf, String)>>,
        revealed: RefCell<Vec<PathBuf>>,
        refuses: bool,
    }

    impl Launcher for Fake {
        fn open_with(&self, path: &Path, app: &str) -> io::Result<()> {
            if self.refuses {
                return Err(io::Error::other("Unable to find application"));
            }
            self.launched.borrow_mut().push((path.to_path_buf(), app.to_owned()));
            Ok(())
        }

        fn reveal(&self, path: &Path) -> io::Result<()> {
            if self.refuses {
                return Err(io::Error::other("Finder is not there"));
            }
            self.revealed.borrow_mut().push(path.to_path_buf());
            Ok(())
        }
    }

    fn settings(editor: Option<&str>, terminal: &str) -> Settings {
        Settings {
            editor: editor.map(str::to_owned),
            terminal: terminal.to_owned(),
        }
    }

    #[test]
    fn the_editor_opens_the_worktree_with_the_app_the_settings_name() {
        let worktree = tempfile::tempdir().expect("a directory");
        let launcher = Fake::default();

        open(
            &launcher,
            &settings(Some("Cursor"), "Terminal"),
            worktree.path(),
            OpenWith::Editor,
        )
        .expect("it opens");

        assert_eq!(
            *launcher.launched.borrow(),
            [(worktree.path().to_path_buf(), "Cursor".to_owned())]
        );
    }

    #[test]
    fn the_terminal_opens_the_worktree_with_the_terminal_the_settings_name() {
        let worktree = tempfile::tempdir().expect("a directory");
        let launcher = Fake::default();

        open(
            &launcher,
            &settings(None, "Ghostty"),
            worktree.path(),
            OpenWith::Terminal,
        )
        .expect("it opens");

        assert_eq!(
            *launcher.launched.borrow(),
            [(worktree.path().to_path_buf(), "Ghostty".to_owned())]
        );
    }

    #[test]
    fn with_no_editor_set_nothing_is_launched_and_the_error_says_so() {
        let worktree = tempfile::tempdir().expect("a directory");
        let launcher = Fake::default();

        let error = open(
            &launcher,
            &settings(None, "Terminal"),
            worktree.path(),
            OpenWith::Editor,
        )
        .expect_err("there is no editor");

        assert!(matches!(error, OpenError::NoEditor), "{error}");
        assert!(launcher.launched.borrow().is_empty());
    }

    #[test]
    fn a_worktree_that_is_gone_is_not_opened_and_the_error_names_its_path() {
        let worktree = tempfile::tempdir().expect("a directory");
        let gone = worktree.path().join("removed");
        let launcher = Fake::default();

        let error = open(&launcher, &settings(Some("Zed"), "Terminal"), &gone, OpenWith::Editor)
            .expect_err("the directory is gone");

        assert!(error.to_string().contains(&gone.display().to_string()), "{error}");
        assert!(launcher.launched.borrow().is_empty());
    }

    #[test]
    fn an_app_that_does_not_open_is_named_in_the_error_with_the_path() {
        let worktree = tempfile::tempdir().expect("a directory");
        let launcher = Fake {
            refuses: true,
            ..Fake::default()
        };

        let error = open(
            &launcher,
            &settings(None, "Ghostty"),
            worktree.path(),
            OpenWith::Terminal,
        )
        .expect_err("the app does not open");

        let message = error.to_string();
        assert!(message.contains("Ghostty"), "{message}");
        assert!(message.contains(&worktree.path().display().to_string()), "{message}");
        assert!(message.contains("Unable to find application"), "{message}");
    }

    #[test]
    fn revealing_hands_the_same_path_to_the_finder() {
        let worktree = tempfile::tempdir().expect("a directory");
        let launcher = Fake::default();

        reveal(&launcher, worktree.path()).expect("it reveals");

        assert_eq!(*launcher.revealed.borrow(), [worktree.path().to_path_buf()]);
    }

    #[test]
    fn revealing_a_worktree_that_is_gone_or_refused_says_so() {
        let worktree = tempfile::tempdir().expect("a directory");
        let gone = worktree.path().join("removed");

        let missing = reveal(&Fake::default(), &gone).expect_err("the directory is gone");
        let refused = reveal(
            &Fake {
                refuses: true,
                ..Fake::default()
            },
            worktree.path(),
        )
        .expect_err("the Finder refuses");

        assert!(missing.to_string().contains(&gone.display().to_string()), "{missing}");
        assert!(refused.to_string().contains("Finder is not there"), "{refused}");
    }

    #[test]
    fn the_window_is_told_the_kind_of_the_error_and_its_message() {
        let no_editor = serde_json::to_value(OpenError::NoEditor).expect("it serializes");

        assert_eq!(no_editor["kind"], "no_editor");
        assert!(
            no_editor["message"]
                .as_str()
                .is_some_and(|message| message.contains("editor"))
        );
    }

    #[test]
    fn the_window_asks_for_the_editor_or_the_terminal_by_name() {
        let editor: OpenWith = serde_json::from_str(r#""editor""#).expect("a kind");
        let terminal: OpenWith = serde_json::from_str(r#""terminal""#).expect("a kind");

        assert!(matches!(editor, OpenWith::Editor));
        assert!(matches!(terminal, OpenWith::Terminal));
    }
}
