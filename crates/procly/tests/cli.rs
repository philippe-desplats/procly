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
        help.contains("3 when it stopped before it became healthy")
            && help.contains("4 when it listened on another port than its Lease")
            && help.contains("124 when it did not"),
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
