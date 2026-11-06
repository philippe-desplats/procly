//! The settings of the app, kept in `settings.json` (`app_config_dir()`), apart from the CLI's `config.toml` which the
//! app never touches.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::{fs, io};

use serde::{Deserialize, Serialize, Serializer};
use tauri::{AppHandle, State};

use crate::atomic;
use crate::failure::{self, AppFailureKind};
use crate::window::{self, MainWindow};

/// The terminal `Open in terminal` uses until the developer picks another one.
const DEFAULT_TERMINAL: &str = "Terminal";

/// What the developer chose. An app is stored as the name `open -a` takes.
///
/// A key the file leaves out keeps its default and a key it holds that this version does not know is ignored, so that
/// a file written by a later version of the app still reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Settings {
    /// The app "Open in editor" uses. None until the developer picks one.
    pub(crate) editor: Option<String>,
    /// The app "Open in terminal" uses.
    pub(crate) terminal: String,
    /// Whether the app shows a Dock icon while its window is open. On unless the developer turns it off.
    pub(crate) show_in_dock: bool,
    /// Whether a failure that enters "Needs you" makes a banner. Off until the developer turns it on, which is the one
    /// moment the app asks for the permission, so it changes through [`SettingsStore::set_notify_on_failure`] only.
    pub(crate) notify_on_failure: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            editor: None,
            terminal: DEFAULT_TERMINAL.to_owned(),
            show_in_dock: true,
            notify_on_failure: false,
        }
    }
}

/// The settings as the rest of the app holds them, with the file they survive in.
#[derive(Debug, Clone)]
pub(crate) struct SettingsStore {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    path: PathBuf,
    settings: Mutex<Settings>,
}

impl SettingsStore {
    /// Reads the settings kept in `path`. A file that is missing, unreadable or not what this app wrote gives the
    /// defaults: the worst that costs is the developer choosing again.
    pub(crate) fn open(path: PathBuf) -> Self {
        let settings = fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            inner: Arc::new(Inner {
                path,
                settings: Mutex::new(settings),
            }),
        }
    }

    fn held(&self) -> MutexGuard<'_, Settings> {
        // Nothing here can leave the settings half written, so a poisoned lock still holds a whole value.
        self.inner.settings.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The settings as they stand.
    pub(crate) fn get(&self) -> Settings {
        self.held().clone()
    }

    /// Applies `settings` at once and writes them to the file. A blank editor means none, a blank terminal means the
    /// default one. The notification switch is left as it stands, since only [`Self::set_notify_on_failure`] changes
    /// it, after the permission was asked.
    ///
    /// # Errors
    ///
    /// When the file cannot be written. The change holds until the app quits.
    pub(crate) fn set(&self, settings: Settings) -> Result<(), SettingsError> {
        // Held while the file is written, so that two changes reach the file in the order they were applied.
        let mut held = self.held();
        let settings = Settings {
            editor: settings
                .editor
                .map(|editor| editor.trim().to_owned())
                .filter(|editor| !editor.is_empty()),
            terminal: match settings.terminal.trim() {
                "" => DEFAULT_TERMINAL.to_owned(),
                terminal => terminal.to_owned(),
            },
            show_in_dock: settings.show_in_dock,
            notify_on_failure: held.notify_on_failure,
        };
        held.clone_from(&settings);
        self.write(&settings)
    }

    /// Turns "Notify on failure" on or off at once and writes it to the file, whatever else the window holds.
    ///
    /// # Errors
    ///
    /// When the file cannot be written. The change holds until the app quits.
    pub(crate) fn set_notify_on_failure(&self, enabled: bool) -> Result<(), SettingsError> {
        let mut held = self.held();
        held.notify_on_failure = enabled;
        self.write(&held)
    }

    fn write(&self, settings: &Settings) -> Result<(), SettingsError> {
        let write = |source| SettingsError::Write {
            path: self.inner.path.clone(),
            source,
        };
        let bytes = serde_json::to_vec_pretty(settings).map_err(|error| write(io::Error::other(error)))?;
        atomic::write(&self.inner.path, &bytes).map_err(write)
    }
}

/// The file the settings are kept in, inside the config directory of the app.
pub(crate) const SETTINGS_FILE: &str = "settings.json";

/// The event the native menu sends the window when Settings is chosen, `⌘,` included.
pub(crate) const OPEN_SETTINGS: &str = "open-settings";

/// The settings as they stand.
#[tauri::command]
#[expect(
    clippy::needless_pass_by_value,
    reason = "Tauri hands managed state to a command as a `State`"
)]
pub(crate) fn get_settings(store: State<'_, SettingsStore>) -> Settings {
    store.get()
}

/// Applies the settings at once and writes them to the file.
///
/// # Errors
///
/// When the file cannot be written. The change holds until the app quits, and the window shows the error.
#[tauri::command]
pub(crate) async fn save_settings(
    settings: Settings,
    app: AppHandle,
    store: State<'_, SettingsStore>,
) -> Result<(), SettingsError> {
    let store = store.inner().clone();
    let held = store.clone();
    // Writing the file blocks, so it leaves the task that serves the window.
    let saved = tauri::async_runtime::spawn_blocking(move || store.set(settings))
        .await
        .map_err(|error| SettingsError::task(&error))?;
    // The change holds even when the file could not be written, so the Dock icon follows it either way.
    let window = MainWindow(&app);
    window::dock_setting_changed(&window, held.get().show_in_dock, window.is_open());
    saved
}

/// The settings could not be written, or the thread that writes them broke. The change holds until the app quits.
#[derive(Debug, thiserror::Error)]
pub(crate) enum SettingsError {
    #[error("cannot save the settings to {}: {source}", path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cannot save the settings: {0}")]
    Task(String),
}

impl SettingsError {
    /// The thread that writes the settings stopped before it could.
    pub(crate) fn task(error: &tauri::Error) -> Self {
        Self::Task(error.to_string())
    }
}

impl Serialize for SettingsError {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        failure::serialize(AppFailureKind::Settings, self, serializer)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::{Settings, SettingsStore};

    fn settings_file() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let path = dir.path().join("settings.json");
        (dir, path)
    }

    #[test]
    fn a_missing_file_gives_the_defaults_terminal_and_no_editor() {
        let (_dir, path) = settings_file();

        let settings = SettingsStore::open(path).get();

        assert_eq!(settings.editor, None);
        assert_eq!(settings.terminal, "Terminal");
        assert_eq!(settings, Settings::default());
    }

    #[test]
    fn a_file_that_is_not_settings_gives_the_defaults() {
        let (_dir, path) = settings_file();
        for unreadable in ["", "not json", "[1, 2]", r#"{"editor": 5}"#, r#"{"terminal": null}"#] {
            fs::write(&path, unreadable).expect("the file is written");

            assert_eq!(
                SettingsStore::open(path.clone()).get(),
                Settings::default(),
                "{unreadable:?}"
            );
        }
    }

    #[test]
    fn a_directory_where_the_file_should_be_gives_the_defaults() {
        let (_dir, path) = settings_file();
        fs::create_dir(&path).expect("a directory");

        assert_eq!(SettingsStore::open(path).get(), Settings::default());
    }

    #[test]
    fn an_unknown_key_is_ignored_and_the_known_ones_are_kept() {
        let (_dir, path) = settings_file();
        fs::write(
            &path,
            r#"{"editor": "Cursor", "terminal": "Ghostty", "from_a_later_version": {"on": true}}"#,
        )
        .expect("the file is written");

        let settings = SettingsStore::open(path).get();

        assert_eq!(settings.editor.as_deref(), Some("Cursor"));
        assert_eq!(settings.terminal, "Ghostty");
    }

    fn chosen(editor: Option<&str>, terminal: &str) -> Settings {
        Settings {
            editor: editor.map(str::to_owned),
            terminal: terminal.to_owned(),
            ..Settings::default()
        }
    }

    #[test]
    fn a_change_is_kept_in_the_file_and_read_back_after_a_restart() {
        let (_dir, path) = settings_file();
        let store = SettingsStore::open(path.clone());

        let saved = store.set(chosen(Some("Cursor"), "Ghostty"));

        assert!(saved.is_ok());
        assert_eq!(store.get(), chosen(Some("Cursor"), "Ghostty"));
        assert_eq!(SettingsStore::open(path).get(), chosen(Some("Cursor"), "Ghostty"));
    }

    #[test]
    fn the_file_is_replaced_whole_and_no_temporary_copy_is_left() {
        let (dir, path) = settings_file();
        let store = SettingsStore::open(path);
        store.set(chosen(Some("Zed"), "Terminal")).expect("the first write");
        store.set(chosen(None, "kitty")).expect("the second write");

        let left: Vec<_> = fs::read_dir(dir.path())
            .expect("the directory is read")
            .map(|entry| entry.expect("an entry").file_name())
            .collect();

        assert_eq!(left, ["settings.json"]);
    }

    #[test]
    fn the_directory_of_the_file_is_created_when_it_is_missing() {
        let (dir, _) = settings_file();
        let path = dir.path().join("config").join("settings.json");

        let saved = SettingsStore::open(path.clone()).set(chosen(Some("Zed"), "Terminal"));

        assert!(saved.is_ok());
        assert!(path.is_file());
    }

    #[test]
    fn a_file_that_cannot_be_written_is_reported_and_the_change_holds_until_the_app_quits() {
        let (dir, _) = settings_file();
        // A file where the directory should be.
        let blocker = dir.path().join("config");
        fs::write(&blocker, "").expect("a file");
        let store = SettingsStore::open(blocker.join("settings.json"));

        let saved = store.set(chosen(Some("Zed"), "Terminal"));

        let message = saved.expect_err("the file cannot be written").to_string();
        assert!(message.contains("settings.json"), "{message}");
        assert_eq!(store.get(), chosen(Some("Zed"), "Terminal"));
    }

    #[test]
    fn a_blank_app_name_means_none_for_the_editor_and_the_default_for_the_terminal() {
        let (_dir, path) = settings_file();
        let store = SettingsStore::open(path);

        store.set(chosen(Some("  "), " \t")).expect("the write");

        assert_eq!(store.get(), Settings::default());
    }

    #[test]
    fn an_app_name_is_kept_without_the_blanks_around_it() {
        let (_dir, path) = settings_file();
        let store = SettingsStore::open(path);

        store
            .set(chosen(Some(" Sublime Text "), " WezTerm\n"))
            .expect("the write");

        assert_eq!(store.get(), chosen(Some("Sublime Text"), "WezTerm"));
    }

    #[test]
    fn the_dock_icon_shows_while_the_window_is_open_until_the_developer_turns_it_off() {
        let (_dir, path) = settings_file();

        assert!(SettingsStore::open(path).get().show_in_dock);
    }

    #[test]
    fn the_dock_setting_persists_in_the_file_and_is_read_back_after_a_restart() {
        let (_dir, path) = settings_file();
        let store = SettingsStore::open(path.clone());

        store
            .set(Settings {
                show_in_dock: false,
                ..Settings::default()
            })
            .expect("the write");

        assert!(!store.get().show_in_dock);
        assert!(!SettingsStore::open(path.clone()).get().show_in_dock);
        let written = fs::read_to_string(path).expect("the file");
        assert!(written.contains("\"show_in_dock\": false"), "{written}");
    }

    #[test]
    fn a_file_written_before_the_dock_setting_existed_keeps_the_icon_on() {
        let (_dir, path) = settings_file();
        fs::write(&path, r#"{"editor": "Zed", "terminal": "iTerm"}"#).expect("the file is written");

        let settings = SettingsStore::open(path).get();

        assert!(settings.show_in_dock);
        assert_eq!(settings.editor.as_deref(), Some("Zed"));
    }

    #[test]
    fn notifications_are_off_until_the_developer_turns_them_on() {
        let (_dir, path) = settings_file();

        assert!(!SettingsStore::open(path).get().notify_on_failure);
    }

    #[test]
    fn the_notification_switch_persists_in_the_file_and_is_read_back_after_a_restart() {
        let (_dir, path) = settings_file();
        let store = SettingsStore::open(path.clone());

        store.set_notify_on_failure(true).expect("the write");

        assert!(store.get().notify_on_failure);
        assert!(SettingsStore::open(path.clone()).get().notify_on_failure);
        let written = fs::read_to_string(path).expect("the file");
        assert!(written.contains("\"notify_on_failure\": true"), "{written}");
    }

    #[test]
    fn saving_the_other_settings_never_turns_notifications_on_or_off() {
        let (_dir, path) = settings_file();
        let store = SettingsStore::open(path);
        store.set(chosen(Some("Zed"), "Terminal")).expect("the write");
        store.set_notify_on_failure(true).expect("the write");

        // A window that saved the switch as off, or on before the permission was asked, changes nothing.
        store
            .set(Settings {
                notify_on_failure: false,
                ..chosen(Some("Cursor"), "Ghostty")
            })
            .expect("the write");
        assert!(store.get().notify_on_failure);
        assert_eq!(store.get().editor.as_deref(), Some("Cursor"));

        store.set_notify_on_failure(false).expect("the write");
        store
            .set(Settings {
                notify_on_failure: true,
                ..chosen(Some("Cursor"), "Ghostty")
            })
            .expect("the write");
        assert!(!store.get().notify_on_failure);
    }

    #[test]
    fn a_file_written_before_the_notification_switch_existed_keeps_notifications_off() {
        let (_dir, path) = settings_file();
        fs::write(&path, r#"{"editor": "Zed", "show_in_dock": false}"#).expect("the file is written");

        let settings = SettingsStore::open(path).get();

        assert!(!settings.notify_on_failure);
        assert!(!settings.show_in_dock);
    }

    #[test]
    fn a_key_the_file_leaves_out_keeps_its_default() {
        let (_dir, path) = settings_file();
        fs::write(&path, r#"{"editor": "Zed"}"#).expect("the file is written");

        let settings = SettingsStore::open(path).get();

        assert_eq!(settings.editor.as_deref(), Some("Zed"));
        assert_eq!(settings.terminal, "Terminal");
    }
}
