use std::process::Command;

#[test]
fn version_flag_prints_the_package_version() {
    let output = Command::new(env!("CARGO_BIN_EXE_procly"))
        .arg("--version")
        .output()
        .expect("the procly binary runs");

    assert!(output.status.success(), "exit status: {}", output.status);
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        concat!("procly ", env!("CARGO_PKG_VERSION"), "\n")
    );
}

#[test]
fn a_usage_error_exits_2() {
    // A command that fails to parse never reaches the Daemon, and the state directory is a temporary one anyway, so
    // a command that did parse would start a Daemon there and never under the real home.
    let state = tempfile::tempdir().expect("create a state dir");
    let output = Command::new(env!("CARGO_BIN_EXE_procly"))
        .env("PROCLY_STATE_DIR", state.path())
        .arg("wait")
        .output()
        .expect("the procly binary runs");

    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn the_help_of_up_lists_its_exit_codes() {
    let output = Command::new(env!("CARGO_BIN_EXE_procly"))
        .args(["up", "--help"])
        .output()
        .expect("the procly binary runs");

    assert!(output.status.success(), "exit status: {}", output.status);
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(
        help.contains("3 when it stopped before it became healthy")
            && help.contains("4 when it listened on another port than its Lease")
            && help.contains("124 when it did not"),
        "{help}"
    );
}

#[test]
fn the_help_of_up_tells_what_a_group_and_no_argument_do() {
    let output = Command::new(env!("CARGO_BIN_EXE_procly"))
        .args(["up", "--help"])
        .output()
        .expect("the procly binary runs");

    assert!(output.status.success(), "exit status: {}", output.status);
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(
        help.contains("reused (via <group>)") && help.contains("Without a Service, start every declared Service"),
        "{help}"
    );
}

#[test]
fn the_help_lists_every_exit_code() {
    let output = Command::new(env!("CARGO_BIN_EXE_procly"))
        .arg("--help")
        .output()
        .expect("the procly binary runs");

    let help = String::from_utf8_lossy(&output.stdout);
    assert!(help.contains("4 when `up` finds a Port mismatch"), "{help}");
}

#[test]
fn the_help_of_worktree_init_lists_its_exit_codes() {
    let output = Command::new(env!("CARGO_BIN_EXE_procly"))
        .args(["worktree", "init", "--help"])
        .output()
        .expect("the procly binary runs");

    assert!(output.status.success(), "exit status: {}", output.status);
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(
        help.contains("the first setup command that failed") && help.contains("125 when Procly itself fails"),
        "{help}"
    );
}

#[test]
fn the_help_of_env_tells_how_to_use_it_and_lists_its_exit_codes() {
    let output = Command::new(env!("CARGO_BIN_EXE_procly"))
        .args(["env", "--help"])
        .output()
        .expect("the procly binary runs");

    assert!(output.status.success(), "exit status: {}", output.status);
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(
        help.contains("eval \"$(procly env web)\"") && help.contains("125 when Procly itself fails"),
        "{help}"
    );
}

#[test]
fn the_help_of_up_offers_to_exclude_through_the_gitignore() {
    let output = Command::new(env!("CARGO_BIN_EXE_procly"))
        .args(["up", "--help"])
        .output()
        .expect("the procly binary runs");

    let help = String::from_utf8_lossy(&output.stdout);
    assert!(help.contains("--gitignore"), "{help}");
}
