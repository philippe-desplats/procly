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
    let output = Command::new(env!("CARGO_BIN_EXE_procly"))
        .arg("up")
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
        help.contains("3 when it stopped before it became healthy") && help.contains("124 when it did not"),
        "{help}"
    );
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
