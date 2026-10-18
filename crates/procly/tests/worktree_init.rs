//! `procly worktree init`: a fresh Worktree becomes ready with one command (ADR 0011).

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{Procly, assert_success, git, git_repository, git_worktree_add, stderr, stdout};

/// A Repository whose versioned `procly.toml` holds `config`, with a second Worktree checked out from it.
/// Returns the Main worktree and the new Worktree.
fn repository_with_worktree(procly: &Procly, config: &str) -> (PathBuf, PathBuf) {
    let main = git_repository(procly.scratch(), "app");
    fs::write(main.join("procly.toml"), config).expect("write procly.toml");
    git(&main, &["add", "procly.toml"]);
    git(
        &main,
        &["commit", "--quiet", "--message", "declare the worktree section"],
    );
    let worktree = git_worktree_add(&main, "feature");
    (main, worktree)
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().expect("the path has a parent")).expect("create the parent");
    fs::write(path, text).expect("write the file");
}

#[test]
fn the_listed_files_and_directories_are_copied_from_the_main_worktree() {
    let procly = Procly::new();
    let (main, worktree) = repository_with_worktree(&procly, "[worktree]\ncopy = ['.env', 'certs']\n");
    write(&main.join(".env"), "SECRET=1\n");
    write(&main.join("certs/local.pem"), "pem\n");
    write(&main.join("certs/nested/key.pem"), "key\n");

    let output = procly.procly_in(&worktree, &["worktree", "init"]);

    assert_success(&output);
    assert_eq!(fs::read_to_string(worktree.join(".env")).unwrap(), "SECRET=1\n");
    assert_eq!(fs::read_to_string(worktree.join("certs/local.pem")).unwrap(), "pem\n");
    assert_eq!(
        fs::read_to_string(worktree.join("certs/nested/key.pem")).unwrap(),
        "key\n"
    );
    assert_eq!(stdout(&output), "copied .env\ncopied certs\n", "{}", stderr(&output));
}

#[test]
fn a_file_the_worktree_already_holds_is_left_alone() {
    let procly = Procly::new();
    let (main, worktree) = repository_with_worktree(&procly, "[worktree]\ncopy = ['.env', 'certs']\n");
    write(&main.join(".env"), "SECRET=main\n");
    write(&main.join("certs/a.pem"), "a from main\n");
    write(&main.join("certs/b.pem"), "b from main\n");
    write(&worktree.join(".env"), "SECRET=mine\n");
    write(&worktree.join("certs/a.pem"), "a mine\n");

    let output = procly.procly_in(&worktree, &["worktree", "init"]);

    assert_success(&output);
    assert_eq!(fs::read_to_string(worktree.join(".env")).unwrap(), "SECRET=mine\n");
    assert_eq!(fs::read_to_string(worktree.join("certs/a.pem")).unwrap(), "a mine\n");
    assert_eq!(
        fs::read_to_string(worktree.join("certs/b.pem")).unwrap(),
        "b from main\n"
    );
    assert_eq!(stdout(&output), "copied certs\n");
}

#[test]
fn a_listed_file_the_main_worktree_lacks_is_reported_and_skipped() {
    let procly = Procly::new();
    let (main, worktree) = repository_with_worktree(&procly, "[worktree]\ncopy = ['.env', '.env.local']\n");
    write(&main.join(".env"), "SECRET=1\n");

    let output = procly.procly_in(&worktree, &["worktree", "init"]);

    assert_success(&output);
    assert_eq!(stdout(&output), "copied .env\n");
    assert_eq!(
        stderr(&output),
        "procly: .env.local is not in the Main worktree, nothing to copy\n"
    );
}

#[test]
fn a_second_init_changes_nothing_and_says_so() {
    let procly = Procly::new();
    let (main, worktree) = repository_with_worktree(&procly, "[worktree]\ncopy = ['.env']\n");
    write(&main.join(".env"), "SECRET=1\n");
    assert_success(&procly.procly_in(&worktree, &["worktree", "init"]));

    let output = procly.procly_in(&worktree, &["worktree", "init"]);

    assert_success(&output);
    assert_eq!(stdout(&output), "nothing to do\n");
    assert_eq!(stderr(&output), "");
}

#[test]
fn init_in_the_main_worktree_copies_nothing_onto_itself() {
    let procly = Procly::new();
    let (main, _) = repository_with_worktree(&procly, "[worktree]\ncopy = ['.env']\n");
    write(&main.join(".env"), "SECRET=1\n");

    let output = procly.procly_in(&main, &["worktree", "init"]);

    assert_success(&output);
    assert_eq!(stdout(&output), "nothing to do\n");
    assert_eq!(fs::read_to_string(main.join(".env")).unwrap(), "SECRET=1\n");
}

#[test]
fn the_worktree_may_be_named_by_a_path_instead_of_the_current_directory() {
    let procly = Procly::new();
    let (main, worktree) = repository_with_worktree(&procly, "[worktree]\ncopy = ['.env']\n");
    write(&main.join(".env"), "SECRET=1\n");
    fs::create_dir_all(worktree.join("src")).unwrap();

    // From an unrelated directory, by an absolute path, then from the Worktree, by a relative path inside it.
    let output = procly.procly(&["worktree", "init", &worktree.to_string_lossy()]);
    assert_success(&output);
    assert_eq!(stdout(&output), "copied .env\n");
    fs::remove_file(worktree.join(".env")).unwrap();
    let output = procly.procly_in(&worktree, &["worktree", "init", "src"]);
    assert_success(&output);
    assert_eq!(stdout(&output), "copied .env\n");
}

#[test]
fn setup_commands_run_once_in_order_at_the_root_of_the_worktree() {
    let procly = Procly::new();
    let config = "[worktree]\nsetup = ['echo first >> setup.log', 'pwd > where; echo \"$FROM_CALLER\" > env; echo second >> setup.log']\n";
    let (_, worktree) = repository_with_worktree(&procly, config);

    let output = procly
        .command()
        .current_dir(&worktree)
        .env("FROM_CALLER", "visible")
        .args(["worktree", "init"])
        .output()
        .expect("the procly binary runs");

    assert_success(&output);
    assert_eq!(
        stdout(&output),
        "ran echo first >> setup.log\nran pwd > where; echo \"$FROM_CALLER\" > env; echo second >> setup.log\n"
    );
    assert_eq!(
        fs::read_to_string(worktree.join("setup.log")).unwrap(),
        "first\nsecond\n"
    );
    assert_eq!(
        fs::read_to_string(worktree.join("where")).unwrap().trim(),
        worktree.to_string_lossy()
    );
    assert_eq!(fs::read_to_string(worktree.join("env")).unwrap(), "visible\n");

    let again = procly.procly_in(&worktree, &["worktree", "init"]);

    assert_success(&again);
    assert_eq!(stdout(&again), "nothing to do\n");
    assert_eq!(
        fs::read_to_string(worktree.join("setup.log")).unwrap(),
        "first\nsecond\n"
    );
}

#[test]
fn a_failing_setup_command_stops_init_with_its_exit_code_and_is_retried() {
    let procly = Procly::new();
    let config = "[worktree]\nsetup = ['echo first >> log', 'test -f ok || { echo attempt >> attempts; echo boom; exit 7; }', 'echo third >> log']\n";
    let (_, worktree) = repository_with_worktree(&procly, config);

    let failed = procly.procly_in(&worktree, &["worktree", "init"]);

    assert_eq!(failed.status.code(), Some(7), "{}", stderr(&failed));
    assert_eq!(stdout(&failed), "ran echo first >> log\n");
    let message = stderr(&failed);
    assert!(
        message.contains("setup command `test -f ok")
            && message.contains("exited with code 7")
            && message.contains("boom"),
        "{message}"
    );
    assert_eq!(fs::read_to_string(worktree.join("log")).unwrap(), "first\n");

    write(&worktree.join("ok"), "");
    let retried = procly.procly_in(&worktree, &["worktree", "init"]);

    assert_success(&retried);
    assert_eq!(stdout(&retried).lines().count(), 2, "{}", stdout(&retried));
    assert_eq!(fs::read_to_string(worktree.join("attempts")).unwrap(), "attempt\n");
    assert_eq!(fs::read_to_string(worktree.join("log")).unwrap(), "first\nthird\n");
}

#[test]
fn a_setup_command_killed_by_a_signal_exits_with_128_plus_the_signal() {
    let procly = Procly::new();
    let (_, worktree) = repository_with_worktree(&procly, "[worktree]\nsetup = ['kill -9 $$']\n");

    let output = procly.procly_in(&worktree, &["worktree", "init"]);

    assert_eq!(output.status.code(), Some(137), "{}", stderr(&output));
    assert!(stderr(&output).contains("killed by signal 9"), "{}", stderr(&output));
}

#[test]
fn two_inits_at_once_run_each_setup_command_once() {
    let procly = Procly::new();
    let (_, worktree) = repository_with_worktree(&procly, "[worktree]\nsetup = ['sleep 1; echo x >> count']\n");

    let first = procly
        .command()
        .current_dir(&worktree)
        .args(["worktree", "init"])
        .spawn_piped();
    let second = procly
        .command()
        .current_dir(&worktree)
        .args(["worktree", "init"])
        .spawn_piped();
    let (first, second) = (first.wait_with_output().unwrap(), second.wait_with_output().unwrap());

    assert_success(&first);
    assert_success(&second);
    let mut printed = [stdout(&first), stdout(&second)];
    printed.sort();
    assert_eq!(printed, ["nothing to do\n", "ran sleep 1; echo x >> count\n"]);
    assert_eq!(fs::read_to_string(worktree.join("count")).unwrap(), "x\n");
}

trait SpawnPiped {
    fn spawn_piped(&mut self) -> std::process::Child;
}

impl SpawnPiped for std::process::Command {
    fn spawn_piped(&mut self) -> std::process::Child {
        self.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("the procly binary runs")
    }
}

#[test]
fn a_copy_entry_outside_the_worktree_is_refused() {
    let procly = Procly::new();
    let (_, worktree) = repository_with_worktree(&procly, "[worktree]\ncopy = ['../elsewhere']\n");

    let output = procly.procly_in(&worktree, &["worktree", "init"]);

    assert_eq!(output.status.code(), Some(125));
    assert!(stderr(&output).contains("\"../elsewhere\""), "{}", stderr(&output));
}

#[test]
fn the_one_line_wrapper_works_with_and_without_the_worktree_path() {
    let procly = Procly::new();
    let (main, worktree) = repository_with_worktree(
        &procly,
        "[worktree]\ncopy = ['.env']\nsetup = ['echo ready >> setup.log']\n",
    );
    write(&main.join(".env"), "SECRET=1\n");
    let wrapper = procly.scratch().join("setup-worktree.sh");
    write(&wrapper, "#!/bin/sh\nexec \"$PROCLY\" worktree init \"$@\"\n");
    let run = |dir: &Path, args: &[&str]| {
        let output = std::process::Command::new("sh")
            .arg(&wrapper)
            .args(args)
            .current_dir(dir)
            .env("PROCLY", env!("CARGO_BIN_EXE_procly"))
            .env("PROCLY_STATE_DIR", procly.state_dir())
            .env("PROCLY_CONFIG_DIR", procly.config_dir())
            .output()
            .expect("the wrapper runs");
        assert_success(&output);
        stdout(&output)
    };

    // The orchestrator passes the new Worktree as the first argument, from wherever it runs.
    assert_eq!(
        run(procly.scratch(), &[&worktree.to_string_lossy()]),
        "copied .env\nran echo ready >> setup.log\n"
    );
    // A hook that already runs inside the Worktree passes nothing.
    assert_eq!(run(&worktree, &[]), "nothing to do\n");
}
