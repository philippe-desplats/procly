//! `procly worktree hooks`: Worktrunk drives `procly worktree init` and `procly worktree release` through a Managed
//! block of `.config/wt.toml`, and `eject` takes it out again (ADR 0008). No test runs the real Worktrunk.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{Procly, assert_success, git_repository, json_succeeded, stderr, stdout};

const OPEN: &str = "# >>> procly managed block: worktrunk >>>";
const CLOSE: &str = "# <<< procly managed block: worktrunk <<<";
const PRE_START: &str = "pre-start = \"procly worktree init\"";
const PRE_REMOVE: &str = "pre-remove = \"procly worktree release\"";
const BLOCK: &str = "# >>> procly managed block: worktrunk >>>\n\
                     pre-start = \"procly worktree init\"\n\
                     pre-remove = \"procly worktree release\"\n\
                     # <<< procly managed block: worktrunk <<<\n";
/// The exit code of a command that found something it must not overwrite, see `docs/exit-codes.md`.
const CONFLICT: i32 = 5;

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().expect("the path has a parent")).expect("create the parent");
    fs::write(path, text).expect("write the file");
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).expect("read the file")
}

fn config(main: &Path) -> PathBuf {
    main.join(".config/wt.toml")
}

fn repository(procly: &Procly) -> PathBuf {
    git_repository(procly.scratch(), "app")
}

#[test]
fn a_missing_file_is_created_with_the_block_between_toml_comment_markers() {
    let procly = Procly::new();
    let main = repository(&procly);

    let output = procly.procly_in(&main, &["worktree", "hooks"]);

    assert_success(&output);
    assert_eq!(read(&config(&main)), BLOCK);
    assert!(
        stdout(&output).contains(&format!("written {}", config(&main).display())),
        "{}",
        stdout(&output)
    );
}

#[test]
fn a_file_with_other_keys_keeps_them_and_stays_valid_toml_with_the_hooks_at_the_root() {
    let procly = Procly::new();
    let main = repository(&procly);
    let mine = "# my hooks\npost-create = \"npm ci\"\n\n[post-start]\nserver = \"npm run dev\"\n";
    write(&config(&main), mine);

    assert_success(&procly.procly_in(&main, &["worktree", "hooks"]));

    let text = read(&config(&main));
    assert!(text.ends_with(mine), "the developer's text is kept as it was: {text}");
    let parsed: toml::Table = text.parse().expect("the file is still TOML");
    assert_eq!(parsed["pre-start"].as_str(), Some("procly worktree init"), "{text}");
    assert_eq!(parsed["pre-remove"].as_str(), Some("procly worktree release"), "{text}");
    assert_eq!(parsed["post-create"].as_str(), Some("npm ci"), "{text}");
    assert_eq!(parsed["post-start"]["server"].as_str(), Some("npm run dev"), "{text}");
}

#[test]
fn running_it_twice_leaves_one_block_and_changes_nothing() {
    let procly = Procly::new();
    let main = repository(&procly);
    write(&config(&main), "post-create = \"npm ci\"\n");
    assert_success(&procly.procly_in(&main, &["worktree", "hooks"]));
    let first = read(&config(&main));

    let output = procly.procly_in(&main, &["worktree", "hooks"]);

    assert_success(&output);
    assert_eq!(read(&config(&main)), first);
    assert_eq!(first.matches(OPEN).count(), 1, "{first}");
    assert_eq!(first.matches(CLOSE).count(), 1, "{first}");
    assert!(stdout(&output).contains("unchanged"), "{}", stdout(&output));
}

#[test]
fn a_hook_already_set_outside_the_block_writes_nothing_prints_only_its_line_and_exits_5() {
    for (existing, printed, not_printed) in [
        ("pre-start = \"npm ci\"\n", vec![PRE_START], vec![PRE_REMOVE]),
        ("pre-remove = \"npm run clean\"\n", vec![PRE_REMOVE], vec![PRE_START]),
        (
            "post-create = \"npm ci\"\n\n[pre-start]\nnpm = \"npm ci\"\n",
            vec![PRE_START],
            vec![PRE_REMOVE],
        ),
        (
            "\"pre-remove\" = \"npm run clean\"\n",
            vec![PRE_REMOVE],
            vec![PRE_START],
        ),
        (
            "pre-start = \"npm ci\"\npre-remove = \"npm run clean\"\n",
            vec![PRE_START, PRE_REMOVE],
            vec![],
        ),
    ] {
        let procly = Procly::new();
        let main = repository(&procly);
        write(&config(&main), existing);

        let output = procly.procly_in(&main, &["worktree", "hooks"]);

        assert_eq!(output.status.code(), Some(CONFLICT), "{existing}: {}", stderr(&output));
        assert_eq!(read(&config(&main)), existing, "nothing is written");
        let lines = stdout(&output);
        for line in printed {
            assert!(
                lines.contains(line),
                "stdout holds the line of a hook that is set: {lines}"
            );
        }
        for line in not_printed {
            assert!(
                !lines.contains(line),
                "stdout holds only the hooks that are set: {lines}"
            );
        }
        assert!(!lines.contains(OPEN), "the lines are not a block: {lines}");
        assert!(stderr(&output).contains("wt.toml"), "{}", stderr(&output));
    }
}

#[test]
fn a_hook_set_only_inside_the_block_is_not_a_conflict() {
    let procly = Procly::new();
    let main = repository(&procly);
    write(&config(&main), &format!("post-create = \"npm ci\"\n{BLOCK}"));

    let output = procly.procly_in(&main, &["worktree", "hooks"]);

    assert_success(&output);
    assert!(stdout(&output).contains("unchanged"), "{}", stdout(&output));
}

#[test]
fn a_block_the_developer_edited_is_left_as_it_is_and_the_command_still_succeeds() {
    let procly = Procly::new();
    let main = repository(&procly);
    assert_success(&procly.procly_in(&main, &["worktree", "hooks"]));
    let edited = read(&config(&main)).replace("procly worktree init", "procly worktree init --json");
    write(&config(&main), &edited);

    let output = procly.procly_in(&main, &["worktree", "hooks"]);

    assert_success(&output);
    assert_eq!(read(&config(&main)), edited);
    assert!(stdout(&output).contains("edited"), "{}", stdout(&output));
}

#[test]
fn a_block_moved_under_a_table_header_is_edited_for_the_command_and_for_doctor() {
    let procly = Procly::new();
    let main = repository(&procly);
    assert_success(&procly.procly_in(&main, &["worktree", "hooks"]));
    // The same lines, after a table header, where their keys belong to that table and hook nothing.
    let moved = format!("[post-start]\nserver = \"npm run dev\"\n\n{BLOCK}");
    write(&config(&main), &moved);

    let output = procly.procly_in(&main, &["worktree", "hooks", "--json"]);

    assert_success(&output);
    assert_eq!(read(&config(&main)), moved, "the file is left as it is");
    assert_eq!(common::json_document(&output)["outcome"], "edited");
    let doctor = common::json_document(&procly.procly_in(&main, &["doctor", "--json"]));
    let check = doctor["checks"]
        .as_array()
        .expect("checks is a list")
        .iter()
        .find(|check| check["name"] == "worktrunk_hooks")
        .expect("a worktrunk_hooks check");
    assert_eq!(check["status"], "failed", "{check}");
    assert!(
        check["message"]
            .as_str()
            .is_some_and(|message| message.contains("table header")),
        "{check}"
    );
}

#[test]
fn a_file_that_is_not_toml_is_a_procly_failure_and_is_left_alone() {
    let procly = Procly::new();
    let main = repository(&procly);
    write(&config(&main), "pre-start = \n[[[\n");

    let output = procly.procly_in(&main, &["worktree", "hooks"]);

    assert_eq!(output.status.code(), Some(125), "{}", stderr(&output));
    assert_eq!(read(&config(&main)), "pre-start = \n[[[\n");
}

#[test]
fn a_directory_outside_a_git_repository_is_a_procly_failure() {
    let procly = Procly::new();
    let elsewhere = procly.scratch().join("plain");
    fs::create_dir_all(&elsewhere).expect("create the directory");

    let output = procly.procly_in(&elsewhere, &["worktree", "hooks"]);

    assert_eq!(output.status.code(), Some(125), "{}", stderr(&output));
}

#[test]
fn the_block_goes_into_the_worktree_the_command_runs_in() {
    let procly = Procly::new();
    let main = repository(&procly);
    let linked = common::git_worktree_add(&main, "app-feature");

    assert_success(&procly.procly_in(&linked, &["worktree", "hooks"]));

    assert_eq!(read(&config(&linked)), BLOCK);
    assert!(!config(&main).exists());
}

#[test]
fn hooks_json_names_the_file_and_what_became_of_the_block() {
    let procly = Procly::new();
    let main = repository(&procly);

    let first = json_succeeded(&procly.procly_in(&main, &["worktree", "hooks", "--json"]));
    let second = json_succeeded(&procly.procly_in(&main, &["worktree", "hooks", "--json"]));

    assert_eq!(first["file"], config(&main).display().to_string());
    assert_eq!(first["outcome"], "written");
    assert_eq!(second["outcome"], "unchanged");
}

#[test]
fn eject_takes_an_intact_block_out_and_gives_the_file_back_as_it_was() {
    let procly = Procly::new();
    let main = repository(&procly);
    let mine = "post-create = \"npm ci\"\n\n[post-start]\nserver = \"npm run dev\"\n";
    write(&config(&main), mine);
    assert_success(&procly.procly_in(&main, &["worktree", "hooks"]));

    let output = procly.procly_in(&main, &["eject"]);

    assert_success(&output);
    assert_eq!(read(&config(&main)), mine);
    assert!(
        stdout(&output).contains(&format!("removed worktrunk hooks {}", config(&main).display())),
        "{}",
        stdout(&output)
    );
}

#[test]
fn eject_deletes_a_file_and_a_directory_that_only_the_block_created() {
    let procly = Procly::new();
    let main = repository(&procly);
    assert_success(&procly.procly_in(&main, &["worktree", "hooks"]));

    assert_success(&procly.procly_in(&main, &["eject"]));

    assert!(!main.join(".config").exists());
}

#[test]
fn eject_removes_the_block_even_when_the_state_of_the_daemon_was_lost() {
    let procly = Procly::new();
    let main = repository(&procly);
    write(&config(&main), "post-create = \"npm ci\"\n");
    assert_success(&procly.procly_in(&main, &["worktree", "hooks"]));
    procly.stop_daemon();
    fs::remove_file(procly.state_dir().join("managed-blocks.json")).expect("remove the fingerprints");

    assert_success(&procly.procly_in(&main, &["eject"]));

    assert_eq!(read(&config(&main)), "post-create = \"npm ci\"\n");
}

#[test]
fn eject_leaves_an_edited_block_and_describes_it_in_the_prompt() {
    let procly = Procly::new();
    let main = repository(&procly);
    assert_success(&procly.procly_in(&main, &["worktree", "hooks"]));
    let edited = read(&config(&main)).replace("procly worktree init", "procly worktree init && make");
    write(&config(&main), &edited);

    let output = procly.procly_in(&main, &["eject"]);

    assert_success(&output);
    assert_eq!(read(&config(&main)), edited);
    let text = stdout(&output);
    assert!(
        text.contains(&format!("left worktrunk hooks {}", config(&main).display())),
        "{text}"
    );
    let (_, prompt) = text.split_once("\n\n").expect("a prompt follows the summary");
    assert!(
        prompt.contains(&format!("{}: the Worktrunk hooks block", config(&main).display())),
        "{prompt}"
    );
    assert!(prompt.contains(OPEN) && prompt.contains(CLOSE), "{prompt}");
    assert!(
        prompt.contains("pre-start") && prompt.contains("pre-remove"),
        "{prompt}"
    );
}

#[test]
fn eject_json_lists_the_block_with_its_kind() {
    let procly = Procly::new();
    let main = repository(&procly);
    assert_success(&procly.procly_in(&main, &["worktree", "hooks"]));

    let document = json_succeeded(&procly.procly_in(&main, &["eject", "--json"]));

    let removed = document["removed"].as_array().expect("removed is a list");
    assert!(
        removed
            .iter()
            .any(|item| item["kind"] == "worktrunk_hooks" && item["deleted"] == true),
        "{document}"
    );
}

#[test]
fn doctor_reports_a_block_whose_fingerprint_no_longer_matches() {
    let procly = Procly::new();
    let main = repository(&procly);
    assert_success(&procly.procly_in(&main, &["worktree", "hooks"]));
    let intact = procly.procly_in(&main, &["doctor", "--json"]);
    let check = |document: &serde_json::Value| {
        document["checks"]
            .as_array()
            .expect("checks is a list")
            .iter()
            .find(|check| check["name"] == "worktrunk_hooks")
            .cloned()
            .expect("a worktrunk_hooks check")
    };
    let intact = check(&common::json_document(&intact));
    assert_eq!(intact["status"], "passed", "{intact}");

    let edited = read(&config(&main)).replace("procly worktree init", "procly worktree init --json");
    write(&config(&main), &edited);
    let output = procly.procly_in(&main, &["doctor", "--json"]);

    let edited = check(&common::json_document(&output));
    assert_eq!(edited["status"], "failed", "{edited}");
    assert!(
        edited["message"]
            .as_str()
            .is_some_and(|message| message.contains("wt.toml")),
        "{edited}"
    );
}

#[test]
fn doctor_skips_the_check_when_the_worktree_has_no_block() {
    let procly = Procly::new();
    let main = repository(&procly);
    write(&config(&main), "post-create = \"npm ci\"\n");

    let output = procly.procly_in(&main, &["doctor", "--json"]);

    let document = common::json_document(&output);
    let check = document["checks"]
        .as_array()
        .expect("checks is a list")
        .iter()
        .find(|check| check["name"] == "worktrunk_hooks")
        .expect("a worktrunk_hooks check");
    assert_eq!(check["status"], "skipped", "{check}");
}

#[test]
fn a_block_that_never_closes_is_left_as_it_is_and_is_no_conflict() {
    let procly = Procly::new();
    let main = repository(&procly);
    let unclosed = format!("{OPEN}\n{PRE_START}\n{PRE_REMOVE}\n");
    write(&config(&main), &unclosed);

    let output = procly.procly_in(&main, &["worktree", "hooks"]);

    assert_success(&output);
    assert_eq!(read(&config(&main)), unclosed);
    assert!(stdout(&output).contains("edited"), "{}", stdout(&output));
}
