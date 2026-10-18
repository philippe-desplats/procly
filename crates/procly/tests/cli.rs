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
