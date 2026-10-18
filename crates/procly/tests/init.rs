//! `procly init`: a Repository gets a `procly.toml` proposed from its package scripts and its compose file.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use common::{Procly, assert_success, git_repository, stderr, stdout};

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().expect("the path has a parent")).expect("create the parent");
    fs::write(path, text).expect("write the file");
}

/// A Repository whose Main worktree holds the given files.
fn repository(procly: &Procly, files: &[(&str, &str)]) -> PathBuf {
    let main = git_repository(procly.scratch(), "app");
    for (path, text) in files {
        write(&main.join(path), text);
    }
    main
}

/// Runs `procly init` in `dir` and asserts it succeeded.
fn init(procly: &Procly, dir: &Path) -> Output {
    let output = procly.procly_in(dir, &["init"]);
    assert_success(&output);
    output
}

/// Asserts the proposal loads: `procly env` reads the configuration the way `up` does, and fails when it is invalid.
fn assert_loads(procly: &Procly, dir: &Path, service: &str) {
    let output = procly.procly_in(dir, &["env", service]);
    assert!(
        output.status.success(),
        "procly env {service} failed: {}\n{}",
        stderr(&output),
        fs::read_to_string(dir.join("procly.toml")).unwrap_or_default()
    );
}

#[test]
fn a_package_with_a_dev_script_becomes_a_process_service() {
    let procly = Procly::new();
    let main = repository(
        &procly,
        &[(
            "package.json",
            r#"{"name": "shop", "scripts": {"dev": "node server.js"}}"#,
        )],
    );

    let output = init(&procly, &main);

    let written = fs::read_to_string(main.join("procly.toml")).expect("procly.toml was written");
    assert_eq!(stdout(&output), written, "stdout holds the proposal");
    assert!(written.contains("[services.shop]"), "{written}");
    assert!(written.contains(r#"run = "npm run dev""#), "{written}");
    assert!(stderr(&output).contains("wrote"), "{}", stderr(&output));
    assert_loads(&procly, &main, "shop");
}

#[test]
fn an_existing_procly_toml_is_never_overwritten() {
    let procly = Procly::new();
    let existing = "[services.mine]\nrun = 'sleep 1000'\n";
    let main = repository(
        &procly,
        &[
            (
                "package.json",
                r#"{"name": "shop", "scripts": {"dev": "node server.js"}}"#,
            ),
            ("procly.toml", existing),
        ],
    );

    let output = init(&procly, &main);

    assert_eq!(fs::read_to_string(main.join("procly.toml")).unwrap(), existing);
    assert!(
        stdout(&output).contains("[services.shop]"),
        "the proposal is still printed"
    );
    assert!(stderr(&output).contains("exists already"), "{}", stderr(&output));
}

#[test]
fn a_worktree_with_no_dev_script_and_no_compose_file_gets_nothing_written() {
    let procly = Procly::new();
    let main = repository(
        &procly,
        &[("package.json", r#"{"name": "lib", "scripts": {"build": "tsc"}}"#)],
    );

    let output = procly.procly_in(&main, &["init"]);

    assert_eq!(output.status.code(), Some(125));
    assert!(stderr(&output).contains("nothing to propose"), "{}", stderr(&output));
    assert!(!main.join("procly.toml").exists());
}

#[test]
fn a_directory_outside_any_repository_is_refused() {
    let procly = Procly::new();
    let outside = procly.scratch().join("not-a-repository");
    fs::create_dir(&outside).unwrap();

    let output = procly.procly_in(&outside, &["init"]);

    assert_eq!(output.status.code(), Some(125));
    assert!(
        stderr(&output).contains("not in a git repository"),
        "{}",
        stderr(&output)
    );
}
