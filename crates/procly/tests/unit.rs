mod common;

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Output;

use common::{Procly, assert_success, json_schema_version, json_succeeded, stdout};

/// A fake `launchctl` or `systemctl` that logs its arguments and remembers whether the unit is loaded, so that no
/// test ever reaches the session manager of the developer.
const FAKE_MANAGER: &str = r#"#!/bin/sh
dir=$(dirname "$0")
echo "$(basename "$0") $*" >> "$dir/calls"
if [ "$1" = "--user" ]; then shift; fi
case "$1" in
    print) [ -f "$dir/loaded" ] || exit 113; echo "state = running" ;;
    bootstrap) touch "$dir/loaded" ;;
    bootout) [ -f "$dir/loaded" ] || exit 3; rm "$dir/loaded" ;;
    enable) touch "$dir/loaded" ;;
    disable) rm -f "$dir/loaded" ;;
    is-enabled) if [ -f "$dir/loaded" ]; then echo enabled; else echo disabled; exit 1; fi ;;
    is-active) if [ -f "$dir/loaded" ]; then echo active; else echo inactive; exit 3; fi ;;
esac
"#;

/// A home directory of its own and fake session managers first in `PATH`.
struct Session {
    home: PathBuf,
    bin: PathBuf,
}

impl Session {
    fn new(procly: &Procly) -> Self {
        let (home, bin) = (procly.scratch().join("home"), procly.scratch().join("bin"));
        std::fs::create_dir_all(&home).expect("create the home directory");
        std::fs::create_dir_all(&bin).expect("create the fake tools directory");
        for tool in ["launchctl", "systemctl"] {
            let path = bin.join(tool);
            std::fs::write(&path, FAKE_MANAGER).expect("write a fake session manager");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("make it executable");
        }
        Self { home, bin }
    }

    fn unit(&self, procly: &Procly, verb: &str) -> Output {
        self.unit_with(procly, &[verb])
    }

    fn unit_with(&self, procly: &Procly, args: &[&str]) -> Output {
        let path = format!("{}:{}", self.bin.display(), std::env::var("PATH").unwrap_or_default());
        procly
            .command()
            .arg("unit")
            .args(args)
            .env("HOME", &self.home)
            .env_remove("XDG_CONFIG_HOME")
            .env("PATH", path)
            .output()
            .expect("the procly binary runs")
    }

    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.bin.join("calls"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

fn unit_path(home: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        home.join("Library/LaunchAgents/io.github.philippe-desplats.procly.plist")
    } else {
        home.join(".config/systemd/user/procly.service")
    }
}

#[test]
fn the_session_unit_is_installed_inspected_and_removed() {
    let procly = Procly::new();
    let session = Session::new(&procly);
    let path = unit_path(&session.home);
    let uid = nix::unistd::getuid();

    let output = session.unit(&procly, "inspect");
    assert_success(&output);
    assert_eq!(
        stdout(&output),
        format!(
            "unit {}\ninstalled no\nstarts with the session no\ndaemon running no\n",
            path.display()
        )
    );

    let before = session.calls().len();
    let output = session.unit(&procly, "install");
    assert_success(&output);
    assert_eq!(stdout(&output), format!("installed {}\n", path.display()));
    let content = std::fs::read_to_string(&path).expect("the unit file is written");
    assert!(content.contains(env!("CARGO_BIN_EXE_procly")), "{content}");
    assert!(content.contains(&procly.state_dir().display().to_string()), "{content}");
    let label = "io.github.philippe-desplats.procly";
    if cfg!(target_os = "macos") {
        assert_eq!(
            session.calls()[before..],
            [
                format!("launchctl bootout gui/{uid}/{label}"),
                format!("launchctl bootstrap gui/{uid} {}", path.display())
            ]
        );
    } else {
        assert_eq!(
            session.calls()[before..],
            [
                "systemctl --user daemon-reload",
                "systemctl --user enable --now procly.service"
            ]
        );
    }

    let output = session.unit(&procly, "inspect");
    assert_success(&output);
    assert_eq!(
        stdout(&output),
        format!(
            "unit {}\ninstalled yes\nstarts with the session yes\ndaemon running yes\n",
            path.display()
        )
    );

    let before = session.calls().len();
    let output = session.unit(&procly, "remove");
    assert_success(&output);
    assert_eq!(stdout(&output), format!("removed {}\n", path.display()));
    assert!(!path.exists(), "the unit file is left behind");
    if cfg!(target_os = "macos") {
        assert_eq!(
            session.calls()[before..],
            [format!("launchctl bootout gui/{uid}/{label}")]
        );
    } else {
        assert_eq!(
            session.calls()[before..],
            [
                "systemctl --user disable --now procly.service",
                "systemctl --user daemon-reload"
            ]
        );
    }

    let output = session.unit(&procly, "remove");
    assert_success(&output);
    assert_eq!(stdout(&output), format!("not installed {}\n", path.display()));
}

#[test]
fn a_failing_session_manager_fails_install_with_125_and_its_error() {
    let procly = Procly::new();
    let session = Session::new(&procly);
    for tool in ["launchctl", "systemctl"] {
        std::fs::write(session.bin.join(tool), "#!/bin/sh\necho 'no session bus' >&2\nexit 5\n").unwrap();
    }

    let output = session.unit(&procly, "install");

    assert_eq!(output.status.code(), Some(125));
    assert!(
        common::stderr(&output).contains("failed: no session bus"),
        "{}",
        common::stderr(&output)
    );
}

#[test]
fn unit_json_carries_the_path_and_what_the_session_manager_says() {
    let procly = Procly::new();
    let session = Session::new(&procly);
    let path = unit_path(&session.home);

    let before = json_succeeded(&session.unit_with(&procly, &["inspect", "--json"]));
    let installed = json_succeeded(&session.unit_with(&procly, &["install", "--json"]));
    let after = json_succeeded(&session.unit_with(&procly, &["inspect", "--json"]));
    let removed = json_succeeded(&session.unit_with(&procly, &["remove", "--json"]));
    let again = json_succeeded(&session.unit_with(&procly, &["remove", "--json"]));

    assert_eq!(before["schema_version"], json_schema_version(&procly));
    assert_eq!(
        before,
        serde_json::json!({
            "schema_version": before["schema_version"],
            "path": path,
            "installed": false,
            "starts_with_session": false,
            "daemon_running": false,
        })
    );
    assert_eq!(installed["path"], serde_json::json!(path));
    assert_eq!(installed["installed"], serde_json::json!(true));
    assert_eq!(after["installed"], serde_json::json!(true));
    assert_eq!(after["starts_with_session"], serde_json::json!(true));
    assert_eq!(after["daemon_running"], serde_json::json!(true));
    assert_eq!(removed["removed"], serde_json::json!(true));
    assert_eq!(again["removed"], serde_json::json!(false));
    assert_eq!(again["path"], serde_json::json!(path));
}
