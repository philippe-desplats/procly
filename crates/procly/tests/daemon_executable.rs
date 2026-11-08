//! The Daemon runs its helpers from the executable it was started from, whatever happens to the file at its path
//! afterwards, as a cask upgrade or a formula upgrade does.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use common::{Procly, assert_success, eventually, free_port, git_repository, stderr, stdout, test_server};

/// The file of the state directory the Daemon runs its helpers from.
const PINNED: &str = "daemon-executable";

/// Copies `source` to `destination` with `cp`, so that no test forks from a process holding the new file open for
/// writing and running it never fails with "Text file busy".
fn copy(source: &Path, destination: &Path) {
    let copied = Command::new("cp")
        .arg(source)
        .arg(destination)
        .status()
        .expect("cp can run");
    assert!(copied.success(), "cp copies the file: {copied}");
}

/// A copy of the `procly` binary under the scratch directory, which stands for the installed executable.
fn installed(procly: &Procly) -> PathBuf {
    let dir = procly.scratch().join("install");
    fs::create_dir_all(&dir).expect("create the install directory");
    let exe = dir.join("procly");
    copy(Path::new(env!("CARGO_BIN_EXE_procly")), &exe);
    exe
}

/// Runs `args` from the executable `exe`, in the state directory of `procly`. The first command starts the Daemon
/// from `exe`.
fn run_from(procly: &Procly, exe: &Path, dir: &Path, args: &[&str]) -> Output {
    Command::new(exe)
        .env("PROCLY_STATE_DIR", procly.state_dir())
        .env("PROCLY_CONFIG_DIR", procly.config_dir())
        .current_dir(dir)
        .args(args)
        .output()
        .expect("the installed procly runs")
}

/// Puts at `path` a program that records in `marker` that it ran and reads its input to the end, in place of whatever
/// was there, as an upgrade does: a new file, so a new inode.
fn replace_with_a_recorder(procly: &Procly, path: &Path, marker: &Path) {
    let script = procly.scratch().join("recorder.sh");
    fs::write(
        &script,
        format!("#!/bin/sh\necho \"$@\" >> '{}'\ncat > /dev/null\n", marker.display()),
    )
    .expect("write the recorder");
    let staged = procly.scratch().join("recorder");
    copy(&script, &staged);
    set_executable(&staged);
    fs::rename(&staged, path).expect("replace the installed executable");
}

fn set_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("make the file executable");
}

/// A Worktree declaring the Service `web` on `port`, and the Daemon started from the installed executable `exe`.
fn daemon_started_from(procly: &Procly, exe: &Path) -> (PathBuf, u16) {
    let main = git_repository(procly.scratch(), "app");
    let port = free_port();
    fs::write(
        main.join("procly.toml"),
        format!(
            "[services.web]\nrun = \"{} --port {port}\"\nports = {{ http = {port} }}\n",
            test_server().display()
        ),
    )
    .expect("write the config file");
    assert_success(&run_from(procly, exe, &main, &["status"]));
    (main, port)
}

fn logs_of_web(procly: &Procly, main: &Path) -> String {
    stdout(&procly.procly_in(main, &["logs", "web"]))
}

#[test]
fn a_service_started_after_the_executable_of_the_daemon_was_replaced_gets_the_original_writer() {
    let procly = Procly::new();
    let exe = installed(&procly);
    let (main, port) = daemon_started_from(&procly, &exe);
    let marker = procly.scratch().join("replacement-ran");

    replace_with_a_recorder(&procly, &exe, &marker);
    assert_success(&procly.procly_in(&main, &["up", "web"]));

    eventually("the output of the Service reaches its logs", || {
        logs_of_web(&procly, &main) == format!("listening on {port}\n")
    });
    assert!(
        !marker.exists(),
        "the replacement ran: {}",
        fs::read_to_string(&marker).unwrap_or_default()
    );
}

#[test]
fn a_service_started_after_the_executable_of_the_daemon_was_removed_still_gets_its_writer() {
    let procly = Procly::new();
    let exe = installed(&procly);
    let (main, port) = daemon_started_from(&procly, &exe);

    fs::remove_file(&exe).expect("remove the installed executable");
    assert_success(&procly.procly_in(&main, &["up", "web"]));

    eventually("the output of the Service reaches its logs", || {
        logs_of_web(&procly, &main) == format!("listening on {port}\n")
    });
    let log = procly.daemon_log();
    assert!(!log.contains("cannot start the writer"), "{log}");
}

#[test]
fn a_writer_the_daemon_can_no_longer_launch_is_reported_with_the_restart_to_run() {
    let procly = Procly::new();
    let exe = installed(&procly);
    let (main, port) = daemon_started_from(&procly, &exe);

    // What the Daemon runs its helpers from is gone, as when something cleans the state directory under it.
    let pinned = procly.state_dir().join(PINNED);
    fs::remove_file(&pinned).expect("remove the executable the Daemon pinned");
    let output = procly.procly_in(&main, &["up", "web"]);

    // The Service itself still runs, appending to its output file directly.
    assert_success(&output);
    assert_eq!(stderr(&output), "");
    eventually("the output of the Service reaches its logs", || {
        logs_of_web(&procly, &main) == format!("listening on {port}\n")
    });
    let log = procly.daemon_log();
    assert!(log.contains(&pinned.display().to_string()), "{log}");
    assert!(log.contains("Restart the daemon with `procly daemon stop`"), "{log}");
}
