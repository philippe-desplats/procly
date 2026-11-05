//! The editors and terminals the Settings screen offers: the ones installed among a fixed list of bundles.
//!
//! An app is named by its bundle without the `.app`, which is what `open -a` takes. On Linux no bundle exists, so
//! nothing is detected and both fields are free text: that is documented, not guaranteed and not tested.

use std::path::PathBuf;

use serde::Serialize;
use tauri::{AppHandle, Manager as _};

/// Where bundles are looked for: the folders of the user and of the system. The system folder of utilities holds
/// Terminal, which is not in `/Applications`.
const SYSTEM_ROOTS: [&str; 2] = ["/Applications", "/System/Applications/Utilities"];

/// The editors the Settings screen knows, in the order it lists them.
const EDITORS: [&str; 17] = [
    "Visual Studio Code",
    "Cursor",
    "Zed",
    "Sublime Text",
    "IntelliJ IDEA",
    "IntelliJ IDEA CE",
    "PyCharm",
    "PyCharm CE",
    "WebStorm",
    "GoLand",
    "RustRover",
    "CLion",
    "PhpStorm",
    "Rider",
    "RubyMine",
    "DataGrip",
    "Xcode",
];

/// The terminals the Settings screen knows, in the order it lists them.
const TERMINALS: [&str; 7] = ["Terminal", "iTerm", "Ghostty", "Warp", "WezTerm", "kitty", "Alacritty"];

/// The apps found installed, by the name `open -a` takes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct DetectedApps {
    pub(crate) editors: Vec<String>,
    pub(crate) terminals: Vec<String>,
}

/// The folders to look in: `/Applications`, the system utilities and `~/Applications`.
pub(crate) fn roots(home: Option<PathBuf>) -> Vec<PathBuf> {
    SYSTEM_ROOTS
        .iter()
        .map(PathBuf::from)
        .chain(home.map(|home| home.join("Applications")))
        .collect()
}

/// The editors and terminals of the lists that have a bundle in one of `roots`.
pub(crate) fn detect(roots: &[PathBuf]) -> DetectedApps {
    let installed = |names: &[&str]| {
        names
            .iter()
            .filter(|name| roots.iter().any(|root| root.join(format!("{name}.app")).is_dir()))
            .map(|name| (*name).to_owned())
            .collect()
    };
    DetectedApps {
        editors: installed(&EDITORS),
        terminals: installed(&TERMINALS),
    }
}

/// The editors and terminals installed on this Mac, for the pop-ups of Settings.
///
/// # Errors
///
/// When the thread that looks stops before it could.
#[tauri::command]
pub(crate) async fn detected_apps(app: AppHandle) -> Result<DetectedApps, String> {
    let roots = roots(app.path().home_dir().ok());
    // Looking at the disk blocks, so it leaves the task that serves the window.
    tauri::async_runtime::spawn_blocking(move || detect(&roots))
        .await
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::{DetectedApps, detect};

    fn nothing() -> DetectedApps {
        DetectedApps {
            editors: vec![],
            terminals: vec![],
        }
    }

    fn install(root: &Path, bundle: &str) {
        fs::create_dir_all(root.join(format!("{bundle}.app"))).expect("a bundle directory");
    }

    #[test]
    fn the_installed_apps_of_the_list_are_found_in_every_root_in_the_order_of_the_list() {
        let system = tempfile::tempdir().expect("a directory");
        let home = tempfile::tempdir().expect("a directory");
        install(system.path(), "Zed");
        install(system.path(), "Visual Studio Code");
        install(system.path(), "Ghostty");
        install(home.path(), "Cursor");
        install(home.path(), "iTerm");

        let found = detect(&[system.path().to_path_buf(), home.path().to_path_buf()]);

        assert_eq!(found.editors, ["Visual Studio Code", "Cursor", "Zed"]);
        assert_eq!(found.terminals, ["iTerm", "Ghostty"]);
    }

    #[test]
    fn an_app_outside_the_list_is_not_offered() {
        let root = tempfile::tempdir().expect("a directory");
        install(root.path(), "Safari");
        install(root.path(), "Notes");

        let found = detect(&[root.path().to_path_buf()]);

        assert_eq!(found, nothing());
    }

    #[test]
    fn an_app_installed_in_two_roots_is_offered_once() {
        let first = tempfile::tempdir().expect("a directory");
        let second = tempfile::tempdir().expect("a directory");
        install(first.path(), "Zed");
        install(second.path(), "Zed");

        let found = detect(&[first.path().to_path_buf(), second.path().to_path_buf()]);

        assert_eq!(found.editors, ["Zed"]);
    }

    #[test]
    fn a_root_that_does_not_exist_and_a_file_named_like_a_bundle_find_nothing() {
        let root = tempfile::tempdir().expect("a directory");
        fs::write(root.path().join("Zed.app"), "").expect("a file");

        let found = detect(&[root.path().join("missing"), root.path().to_path_buf()]);

        assert_eq!(found, nothing());
    }

    #[test]
    fn the_jetbrains_ides_and_xcode_are_editors_and_the_other_terminals_are_known() {
        let root = tempfile::tempdir().expect("a directory");
        for bundle in [
            "IntelliJ IDEA",
            "PyCharm CE",
            "WebStorm",
            "GoLand",
            "RustRover",
            "CLion",
            "Xcode",
            "Sublime Text",
            "Terminal",
            "Warp",
            "WezTerm",
            "kitty",
            "Alacritty",
        ] {
            install(root.path(), bundle);
        }

        let found = detect(&[root.path().to_path_buf()]);

        assert_eq!(
            found.editors,
            [
                "Sublime Text",
                "IntelliJ IDEA",
                "PyCharm CE",
                "WebStorm",
                "GoLand",
                "RustRover",
                "CLion",
                "Xcode"
            ]
        );
        assert_eq!(found.terminals, ["Terminal", "Warp", "WezTerm", "kitty", "Alacritty"]);
    }
}
