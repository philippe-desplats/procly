//! Open at login: whether macOS starts the app when the developer logs in.
//!
//! The state lives in the system (a `LaunchAgent` that `tauri-plugin-autostart` writes), never in the settings of the
//! app. It is read from the system every time, so that a login item the developer removed in System Settings shows as
//! off. The switch goes through a [`LoginItem`] so that it is tested without writing a `LaunchAgent`.

use serde::{Serialize, Serializer};
use tauri::AppHandle;
use tauri_plugin_autostart::{AutoLaunchManager, ManagerExt as _};

use crate::window::HIDDEN_FLAG;

/// The switch of the login item, as the system holds it.
pub(crate) trait LoginItem {
    /// Whether the system starts the app at login.
    fn is_enabled(&self) -> Result<bool, LoginItemError>;
    fn enable(&self) -> Result<(), LoginItemError>;
    fn disable(&self) -> Result<(), LoginItemError>;
}

/// The login item could not be read or changed.
#[derive(Debug, thiserror::Error)]
#[error("cannot change Open at login: {0}")]
pub(crate) struct LoginItemError(String);

impl LoginItemError {
    /// The thread that talks to the system stopped before it could.
    pub(crate) fn task(error: &tauri::Error) -> Self {
        Self(error.to_string())
    }
}

impl Serialize for LoginItemError {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Failure {
            kind: &'static str,
            message: String,
        }
        Failure {
            kind: "login_item",
            message: self.to_string(),
        }
        .serialize(serializer)
    }
}

impl LoginItem for AutoLaunchManager {
    fn is_enabled(&self) -> Result<bool, LoginItemError> {
        Self::is_enabled(self).map_err(|error| LoginItemError(error.to_string()))
    }

    fn enable(&self) -> Result<(), LoginItemError> {
        Self::enable(self).map_err(|error| LoginItemError(error.to_string()))
    }

    fn disable(&self) -> Result<(), LoginItemError> {
        Self::disable(self).map_err(|error| LoginItemError(error.to_string()))
    }
}

/// Whether Open at login is on, as the system says.
pub(crate) fn read(item: &impl LoginItem) -> Result<bool, LoginItemError> {
    item.is_enabled()
}

/// Turns Open at login on or off, and returns what the system says afterwards.
pub(crate) fn switch(item: &impl LoginItem, enabled: bool) -> Result<bool, LoginItemError> {
    if item.is_enabled()? != enabled {
        if enabled {
            item.enable()?;
        } else {
            item.disable()?;
        }
    }
    item.is_enabled()
}

/// The plugin of the login item: a `LaunchAgent` on macOS, never `AppleScript`, which starts the app hidden.
pub(crate) fn plugin() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    tauri_plugin_autostart::init(
        tauri_plugin_autostart::MacosLauncher::LaunchAgent,
        Some(vec![HIDDEN_FLAG]),
    )
}

/// Whether Open at login is on, as the system says.
///
/// # Errors
///
/// When the system cannot be read.
#[tauri::command]
pub(crate) async fn get_open_at_login(app: AppHandle) -> Result<bool, LoginItemError> {
    // The system is asked from a thread that may block.
    tauri::async_runtime::spawn_blocking(move || read(&*app.autolaunch()))
        .await
        .map_err(|error| LoginItemError::task(&error))?
}

/// Turns Open at login on or off, and answers what the system says afterwards.
///
/// # Errors
///
/// When the system cannot be changed.
#[tauri::command]
pub(crate) async fn set_open_at_login(enabled: bool, app: AppHandle) -> Result<bool, LoginItemError> {
    tauri::async_runtime::spawn_blocking(move || switch(&*app.autolaunch(), enabled))
        .await
        .map_err(|error| LoginItemError::task(&error))?
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use super::*;

    /// A system with a login item that something else may change, which keeps the calls made on it.
    #[derive(Default)]
    struct System {
        enabled: Cell<bool>,
        calls: RefCell<Vec<&'static str>>,
        refuses: Cell<bool>,
    }

    impl LoginItem for System {
        fn is_enabled(&self) -> Result<bool, LoginItemError> {
            Ok(self.enabled.get())
        }

        fn enable(&self) -> Result<(), LoginItemError> {
            self.calls.borrow_mut().push("enable");
            if self.refuses.get() {
                return Err(LoginItemError("denied".to_owned()));
            }
            self.enabled.set(true);
            Ok(())
        }

        fn disable(&self) -> Result<(), LoginItemError> {
            self.calls.borrow_mut().push("disable");
            self.enabled.set(false);
            Ok(())
        }
    }

    #[test]
    fn open_at_login_is_off_while_the_system_has_no_login_item() {
        let system = System::default();

        assert!(!read(&system).expect("a reading"));
    }

    #[test]
    fn open_at_login_reads_its_state_from_the_system_every_time() {
        let system = System::default();
        assert!(!read(&system).expect("a reading"));

        // The developer adds the login item outside the app, then removes it in System Settings.
        system.enabled.set(true);
        assert!(read(&system).expect("a reading"));
        system.enabled.set(false);
        assert!(!read(&system).expect("a reading"));
    }

    #[test]
    fn turning_it_on_enables_the_login_item_and_answers_what_the_system_says() {
        let system = System::default();

        let now = switch(&system, true).expect("the switch");

        assert!(now);
        assert_eq!(*system.calls.borrow(), ["enable"]);
    }

    #[test]
    fn turning_it_off_disables_the_login_item() {
        let system = System::default();
        system.enabled.set(true);

        let now = switch(&system, false).expect("the switch");

        assert!(!now);
        assert_eq!(*system.calls.borrow(), ["disable"]);
    }

    #[test]
    fn a_login_item_that_is_already_in_the_wanted_state_is_left_alone() {
        let system = System::default();
        system.enabled.set(true);

        switch(&system, true).expect("the switch");

        assert!(system.calls.borrow().is_empty());
    }

    #[test]
    fn a_system_that_refuses_is_reported_and_the_state_shown_stays_the_system_s() {
        let system = System::default();
        system.refuses.set(true);

        let error = switch(&system, true).expect_err("the system refuses");

        assert_eq!(error.to_string(), "cannot change Open at login: denied");
        assert!(!read(&system).expect("a reading"));
    }

    #[test]
    fn the_settings_file_never_holds_the_login_item() {
        let settings = serde_json::to_string(&crate::settings::Settings::default()).expect("the settings");

        assert!(!settings.contains("login"), "{settings}");
    }
}
