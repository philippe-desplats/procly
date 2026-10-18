mod common;

use std::path::Path;
use std::process::Stdio;

use common::{Procly, assert_success, git_repository, git_worktree_add, stdout};
use serde_json::{Value, json};

fn status_json(procly: &Procly, dir: &Path, args: &[&str]) -> Value {
    let output = procly
        .command()
        .current_dir(dir)
        .args([&["status", "--json"], args].concat())
        .output()
        .expect("the procly binary runs");
    assert_success(&output);
    serde_json::from_str(&stdout(&output)).unwrap_or_else(|error| panic!("{error}: {}", stdout(&output)))
}

#[test]
fn two_worktrees_of_one_repository_are_reported_under_it_with_the_main_worktree_identified() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let feature = git_worktree_add(&main, "app-Feature.2");

    let status = status_json(&procly, &feature, &["--all"]);

    assert_eq!(status["schema_version"], 1);
    assert_eq!(
        status["repositories"],
        json!([{
            "git_common_dir": main.join(".git"),
            "worktrees": [
                {"path": main, "slug": "app", "main": true, "jobs": []},
                {"path": feature, "slug": "app_feature_2", "main": false, "jobs": []},
            ],
        }])
    );
}

fn start_in(procly: &Procly, dir: &Path, command: &str) -> (String, String) {
    let output = procly
        .command()
        .current_dir(dir)
        .args(["run", command])
        .output()
        .expect("the procly binary runs");
    assert_success(&output);
    common::started(&output)
}

#[test]
fn the_same_command_in_the_same_worktree_and_directory_returns_the_running_job() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");

    let (first, outcome) = start_in(&procly, &main, "sleep 30");
    assert_eq!(outcome, "started");
    let (second, outcome) = start_in(&procly, &main, "sleep 30");

    assert_eq!(outcome, "reused");
    assert_eq!(second, first);
    let status = status_json(&procly, &main, &[]);
    let jobs = &status["repositories"][0]["worktrees"][0]["jobs"];
    assert_eq!(jobs.as_array().map(Vec::len), Some(1), "{status}");
    assert_eq!(jobs[0]["state"], "running");
}

#[test]
fn the_same_command_in_another_worktree_or_directory_starts_a_separate_job() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let feature = git_worktree_add(&main, "app-feature");
    let subdir = main.join("crates");
    std::fs::create_dir(&subdir).unwrap();

    let (in_main, _) = start_in(&procly, &main, "sleep 30");
    let (in_feature, outcome) = start_in(&procly, &feature, "sleep 30");
    assert_eq!(outcome, "started");
    let (in_subdir, outcome) = start_in(&procly, &subdir, "sleep 30");
    assert_eq!(outcome, "started");

    assert_ne!(in_feature, in_main);
    assert_ne!(in_subdir, in_main);
    let status = status_json(&procly, &feature, &["--all"]);
    let worktrees = &status["repositories"][0]["worktrees"];
    assert_eq!(worktrees[0]["jobs"][0]["id"], in_main.as_str(), "{status}");
    assert_eq!(worktrees[0]["jobs"][1]["id"], in_subdir.as_str(), "{status}");
    assert_eq!(worktrees[1]["jobs"][0]["id"], in_feature.as_str(), "{status}");
}

#[test]
fn a_job_that_has_ended_is_not_reused() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");

    let (first, _) = start_in(&procly, &main, "true");
    assert_success(&procly.procly(&["wait", &first]));
    let (second, outcome) = start_in(&procly, &main, "true");

    assert_eq!(outcome, "started");
    assert_ne!(second, first);
}

#[test]
fn concurrent_runs_of_one_command_share_one_job() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");

    let runs: Vec<_> = (0..6)
        .map(|_| {
            procly
                .command()
                .current_dir(&main)
                .args(["run", "sleep 30"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    let mut outcomes: Vec<(String, String)> = runs
        .into_iter()
        .map(|run| {
            let output = run.wait_with_output().unwrap();
            assert_success(&output);
            common::started(&output)
        })
        .collect();

    outcomes.sort_by(|a, b| b.1.cmp(&a.1));
    assert_eq!(
        outcomes.iter().filter(|(_, outcome)| outcome == "started").count(),
        1,
        "{outcomes:?}"
    );
    assert!(outcomes.iter().all(|(job, _)| *job == outcomes[0].0), "{outcomes:?}");
}

#[test]
fn a_job_outside_any_repository_is_deduplicated_by_its_directory_and_command() {
    let procly = Procly::new();

    let (first, _) = start_in(&procly, procly.scratch(), "sleep 30");
    let (second, outcome) = start_in(&procly, procly.scratch(), "sleep 30");

    assert_eq!((second.as_str(), outcome.as_str()), (first.as_str(), "reused"));
    let status = status_json(&procly, procly.scratch(), &[]);
    assert_eq!(status["repositories"], json!([]));
    assert_eq!(status["jobs_outside_repositories"][0]["id"], first.as_str());
}

#[test]
fn status_prints_compact_text_by_default_even_when_piped() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let feature = git_worktree_add(&main, "app-feature");
    let (job, _) = start_in(&procly, &feature, "sleep 30");

    // `output()` pipes stdout, so the command sees no terminal.
    let output = procly.command().current_dir(&feature).arg("status").output().unwrap();

    assert_success(&output);
    assert_eq!(
        stdout(&output),
        format!(
            "repository {}\n  worktree app_feature {}\n    job {job} running: sleep 30\n",
            main.join(".git").display(),
            feature.display()
        )
    );
}

#[test]
fn a_job_starts_even_where_git_fails_to_resolve_the_topology() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");

    // Inside the git directory, git knows the Repository but finds no Worktree to report.
    let (job, outcome) = start_in(&procly, &main.join(".git"), "echo inside");

    assert_eq!(outcome, "started");
    assert_eq!(stdout(&procly.procly(&["wait", &job])), "inside\n");
}
