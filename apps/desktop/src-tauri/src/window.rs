//! Opening and closing the one window, and the Dock icon that follows it.
//!
//! The app has no Dock icon unless its window is open (`LSUIElement` is `true`), and closing the window leaves the
//! app in the menu bar. The steps go through a [`Host`] so that their order is tested without a window.

use tauri::{AppHandle, Manager as _};

use crate::settings::SettingsStore;

/// The label of the one window, as `tauri.conf.json` names it.
pub(crate) const MAIN: &str = "main";

/// The argument Open at login starts the app with: no window, only the glyph in the menu bar.
pub(crate) const HIDDEN_FLAG: &str = "--hidden";

/// Whether the app was launched with [`HIDDEN_FLAG`], given its arguments after the executable.
pub(crate) fn launched_hidden(args: impl IntoIterator<Item = String>) -> bool {
    args.into_iter().any(|arg| arg == HIDDEN_FLAG)
}

/// What the app does to its window and its Dock icon.
pub(crate) trait Host {
    /// Shows or hides the Dock icon. Only macOS has one.
    fn set_dock_visibility(&self, visible: bool);
    fn show(&self);
    fn focus(&self);
    fn hide(&self);
}

/// Opens the window and takes focus. The Dock icon shows first when the developer wants it while the window is
/// open, then the window shows and takes focus, in that order. With the setting off the app never shows a Dock icon
/// and the window still opens.
pub(crate) fn open(host: &impl Host, show_in_dock: bool) {
    host.set_dock_visibility(show_in_dock);
    host.show();
    host.focus();
}

/// Closes the last window, which leaves the app in the menu bar with no Dock icon.
pub(crate) fn close(host: &impl Host) {
    host.hide();
    host.set_dock_visibility(false);
}

/// The developer changed "Show in Dock while the window is open". It applies at once when the window is open, and
/// waits for the next time the window opens when it is not.
pub(crate) fn dock_setting_changed(host: &impl Host, show_in_dock: bool, window_is_open: bool) {
    if window_is_open {
        host.set_dock_visibility(show_in_dock);
    }
}

/// The window of the app as the one [`Host`] that is not a test.
#[derive(Debug)]
pub(crate) struct MainWindow<'a>(pub(crate) &'a AppHandle);

impl MainWindow<'_> {
    /// Whether the window is shown.
    pub(crate) fn is_open(&self) -> bool {
        self.0
            .get_webview_window(MAIN)
            .is_some_and(|window| window.is_visible().unwrap_or(false))
    }

    /// Opens the window the way the developer's settings say.
    pub(crate) fn open(&self) {
        let show_in_dock = self
            .0
            .try_state::<SettingsStore>()
            .is_none_or(|settings| settings.get().show_in_dock);
        open(self, show_in_dock);
    }
}

impl Host for MainWindow<'_> {
    fn set_dock_visibility(&self, visible: bool) {
        #[cfg(target_os = "macos")]
        if let Err(error) = self.0.set_dock_visibility(visible) {
            eprintln!("procly: cannot change the Dock icon: {error}");
        }
        #[cfg(not(target_os = "macos"))]
        let _ = visible;
    }

    fn show(&self) {
        if let Some(window) = self.0.get_webview_window(MAIN)
            && let Err(error) = window.show()
        {
            eprintln!("procly: cannot show the window: {error}");
        }
    }

    fn focus(&self) {
        if let Some(window) = self.0.get_webview_window(MAIN)
            && let Err(error) = window.set_focus()
        {
            eprintln!("procly: cannot focus the window: {error}");
        }
    }

    fn hide(&self) {
        if let Some(window) = self.0.get_webview_window(MAIN)
            && let Err(error) = window.hide()
        {
            eprintln!("procly: cannot hide the window: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    /// A host that records what it was asked, in order.
    #[derive(Default)]
    struct Recorder(RefCell<Vec<String>>);

    impl Recorder {
        fn calls(&self) -> Vec<String> {
            self.0.borrow().clone()
        }

        fn note(&self, call: &str) {
            self.0.borrow_mut().push(call.to_owned());
        }
    }

    impl Host for Recorder {
        fn set_dock_visibility(&self, visible: bool) {
            self.note(&format!("set_dock_visibility({visible})"));
        }

        fn show(&self) {
            self.note("show");
        }

        fn focus(&self) {
            self.note("set_focus");
        }

        fn hide(&self) {
            self.note("hide");
        }
    }

    #[test]
    fn opening_with_the_dock_on_shows_the_icon_then_the_window_then_takes_focus() {
        let host = Recorder::default();

        open(&host, true);

        assert_eq!(host.calls(), ["set_dock_visibility(true)", "show", "set_focus"]);
    }

    #[test]
    fn opening_with_the_dock_off_never_shows_the_icon_and_the_window_still_takes_focus() {
        let host = Recorder::default();

        open(&host, false);

        assert_eq!(host.calls(), ["set_dock_visibility(false)", "show", "set_focus"]);
    }

    #[test]
    fn closing_the_last_window_takes_the_icon_out_of_the_dock() {
        let host = Recorder::default();

        close(&host);

        assert_eq!(host.calls(), ["hide", "set_dock_visibility(false)"]);
    }

    #[test]
    fn a_change_of_the_dock_setting_applies_at_once_while_the_window_is_open() {
        let host = Recorder::default();

        dock_setting_changed(&host, false, true);
        dock_setting_changed(&host, true, true);

        assert_eq!(
            host.calls(),
            ["set_dock_visibility(false)", "set_dock_visibility(true)"]
        );
    }

    #[test]
    fn a_change_of_the_dock_setting_waits_for_the_window_while_it_is_closed() {
        let host = Recorder::default();

        dock_setting_changed(&host, true, false);

        assert_eq!(host.calls(), Vec::<String>::new());
    }

    #[test]
    fn the_app_is_an_agent_app_so_that_a_launch_starts_with_no_dock_icon() {
        let plist: String = include_str!("../Info.plist").split_whitespace().collect();

        assert!(plist.contains("<key>LSUIElement</key><true/>"), "{plist}");
    }

    #[test]
    fn the_app_launched_by_open_at_login_is_told_to_stay_hidden() {
        let from_login = ["--hidden".to_owned()];
        let by_hand: [String; 0] = [];

        assert!(launched_hidden(from_login));
        assert!(!launched_hidden(by_hand));
        assert!(!launched_hidden(["--hidden-not".to_owned(), "hidden".to_owned()]));
    }
}
