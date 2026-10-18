//! `procly eject`: everything Procly wrote into a Repository is removed, except the Managed blocks the developer
//! edited, which a generated prompt describes (ADR 0008).

mod common;

use std::fs;
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Output;

use common::{
    Procly, assert_success, eventually, free_port, git, git_output, git_repository, git_worktree_add, stderr, stdout,
    test_server,
};

const RULES_OPEN: &str = "<!-- >>> procly managed block: rules >>> -->";
const INSTRUCTIONS: &str = "# App\n\nBuild with make.\n";

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().expect("the path has a parent")).expect("create the parent");
    fs::write(path, text).expect("write the file");
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).expect("read the file")
}

/// The files of the Repository as they sit on disk before Procly, apart from `.git`.
struct Before {
    instructions: String,
    exclude: String,
}

/// A committed Repository with an `AGENTS.md` and a Service `web`, and what it held before Procly touched it.
fn repository(procly: &Procly) -> (PathBuf, u16, Before) {
    let main = git_repository(procly.scratch(), "app");
    let port = free_port();
    write(&main.join("AGENTS.md"), INSTRUCTIONS);
    write(
        &main.join("procly.toml"),
        &format!(
            "[services.web]\nrun = '{} --port {{ports.http}}'\nports = {{ http = {port} }}\n",
            test_server().display()
        ),
    );
    git(&main, &["add", "."]);
    git(&main, &["commit", "--quiet", "--message", "app"]);
    let before = Before {
        instructions: read(&main.join("AGENTS.md")),
        exclude: read(&main.join(".git/info/exclude")),
    };
    (main, port, before)
}

/// Writes the Rules block and the Guard, then starts `web`, which writes the Env file and the exclusions.
fn adopt_procly(procly: &Procly, main: &Path) {
    assert_success(&procly.procly_in(main, &["agent", "install"]));
    assert_success(&procly.procly_in(main, &["up", "web"]));
}

fn eject(procly: &Procly, dir: &Path) -> Output {
    let output = procly.procly_in(dir, &["eject"]);
    assert_success(&output);
    output
}

fn listens(port: u16) -> bool {
    TcpStream::connect(("127.0.0.1", port)).is_ok()
}

#[test]
fn after_eject_the_tracked_files_and_the_exclude_file_are_what_they_were_before_procly() {
    let procly = Procly::new();
    let (main, _, before) = repository(&procly);
    adopt_procly(&procly, &main);
    assert!(read(&main.join("AGENTS.md")).contains(RULES_OPEN));
    assert!(main.join(".claude/settings.json").exists());
    assert!(main.join(".env.procly").exists());

    let output = eject(&procly, &main);

    assert_eq!(read(&main.join("AGENTS.md")), before.instructions);
    assert_eq!(read(&main.join(".git/info/exclude")), before.exclude);
    assert!(!main.join(".claude").exists(), "the Guard file held nothing else");
    assert!(!main.join(".env.procly").exists());
    assert_eq!(git_output(&main, &["status", "--porcelain"]), "");
    let summary = stdout(&output);
    for removed in ["rules", "guard", "exclusions", "env file"] {
        assert!(
            summary
                .lines()
                .any(|line| line.starts_with(&format!("removed {removed}"))),
            "{removed} is not listed as removed: {summary}"
        );
    }
}

/// The lines of the summary, which come before the prompt.
fn summary(output: &Output) -> Vec<String> {
    stdout(output)
        .lines()
        .take_while(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

#[test]
fn a_managed_block_the_developer_edited_is_left_untouched_and_described_in_the_prompt() {
    let procly = Procly::new();
    let (main, _, before) = repository(&procly);
    adopt_procly(&procly, &main);
    let edited_rules = read(&main.join("AGENTS.md")).replace("Never start", "Please avoid starting");
    write(&main.join("AGENTS.md"), &edited_rules);
    let settings = read(&main.join(".claude/settings.json")).replace("\"Bash\"", "\"Bash|Task\"");
    write(&main.join(".claude/settings.json"), &settings);
    let exclude = read(&main.join(".git/info/exclude")).replace("/procly.local.toml\n", "/procly.local.toml\n/mine\n");
    write(&main.join(".git/info/exclude"), &exclude);
    write(&main.join(".env.procly"), "MY_OWN=1\n");

    let output = eject(&procly, &main);

    assert_eq!(read(&main.join("AGENTS.md")), edited_rules);
    assert_eq!(read(&main.join(".claude/settings.json")), settings);
    assert_eq!(read(&main.join(".git/info/exclude")), exclude);
    assert_eq!(read(&main.join(".env.procly")), "MY_OWN=1\n");
    let summary = summary(&output);
    for kind in ["rules", "guard", "exclusions", "env file"] {
        assert!(
            summary.iter().any(|line| line.starts_with(&format!("left {kind} "))),
            "{kind} is not listed as left: {summary:?}"
        );
        assert!(
            !summary.iter().any(|line| line.starts_with(&format!("removed {kind} "))),
            "{summary:?}"
        );
    }
    let prompt = stdout(&output);
    let (_, prompt) = prompt.split_once("\n\n").expect("a prompt follows the summary");
    for item in [
        format!("{}: the Rules block", main.join("AGENTS.md").display()),
        format!("{}: the Guard", main.join(".claude/settings.json").display()),
        format!(
            "{}: the Managed block that excludes",
            main.join(".git/info/exclude").display()
        ),
        format!("{}: the Env file", main.join(".env.procly").display()),
    ] {
        assert!(prompt.contains(&item), "{item} is not in the prompt: {prompt}");
    }
    assert!(stderr(&output).contains("4 items were edited"), "{}", stderr(&output));
    // What the developer wrote stays where it was before Procly, nothing else of the Repository changed.
    assert!(before.instructions.len() < edited_rules.len());
}

#[test]
fn a_second_eject_finds_nothing_to_remove() {
    let procly = Procly::new();
    let (main, _, _) = repository(&procly);
    adopt_procly(&procly, &main);
    eject(&procly, &main);

    let output = eject(&procly, &main);

    assert_eq!(stdout(&output), "left Docker volumes and Logical resources\n");
    assert_eq!(stderr(&output), "");
}

#[test]
fn every_worktree_and_every_file_procly_wrote_in_is_cleaned() {
    let procly = Procly::new();
    let (main, _, before) = repository(&procly);
    let other = git_worktree_add(&main, "app-other");
    // A Rules block in a named file, a symlinked CLAUDE.md, and the exclusions in a `.gitignore`.
    assert_success(&procly.procly_in(&main, &["agent", "install", "--file", "docs/agents.md"]));
    std::os::unix::fs::symlink("AGENTS.md", other.join("CLAUDE.md")).expect("link CLAUDE.md");
    git(&other, &["rm", "--quiet", "--force", "AGENTS.md"]);
    write(&other.join("AGENTS.md"), INSTRUCTIONS);
    assert_success(&procly.procly_in(&other, &["agent", "install"]));
    assert_success(&procly.procly_in(&main, &["up", "--gitignore", "web"]));
    assert!(read(&main.join(".gitignore")).contains("procly managed block: exclusions"));
    assert!(other.join(".claude/settings.json").exists());

    let output = eject(&procly, &other);

    assert!(
        !main.join("docs").exists(),
        "the named file held nothing else, nor did its directory"
    );
    assert!(!main.join(".gitignore").exists());
    assert!(!main.join(".env.procly").exists());
    assert!(!other.join(".claude").exists());
    assert_eq!(read(&other.join("AGENTS.md")), INSTRUCTIONS);
    assert_eq!(
        fs::read_link(other.join("CLAUDE.md")).expect("CLAUDE.md is still a link"),
        Path::new("AGENTS.md")
    );
    assert_eq!(read(&main.join(".git/info/exclude")), before.exclude);
    assert!(stdout(&output).contains(&format!("removed rules {}", main.join("docs/agents.md").display())));
    assert!(stdout(&output).contains(&format!("removed exclusions {}", main.join(".gitignore").display())));
}

#[test]
fn blocks_are_removed_even_when_the_state_of_the_daemon_was_lost() {
    let procly = Procly::new();
    let (main, _, before) = repository(&procly);
    adopt_procly(&procly, &main);
    procly.stop_daemon();
    fs::remove_file(procly.state_dir().join("managed-blocks.json")).expect("remove the fingerprints");

    eject(&procly, &main);

    assert_eq!(read(&main.join("AGENTS.md")), before.instructions);
    assert_eq!(read(&main.join(".git/info/exclude")), before.exclude);
    assert!(!main.join(".claude").exists());
}

#[test]
fn the_hooks_and_settings_of_the_developer_stay_when_the_guard_goes() {
    let procly = Procly::new();
    let (main, _, _) = repository(&procly);
    let own = "{\n  \"model\": \"opus\",\n  \"hooks\": {\n    \"PreToolUse\": [\n      {\n        \"matcher\": \"Edit\",\n        \"hooks\": [\n          {\n            \"type\": \"command\",\n            \"command\": \"lint\"\n          }\n        ]\n      }\n    ]\n  }\n}\n";
    write(&main.join(".claude/settings.json"), own);
    adopt_procly(&procly, &main);
    assert!(read(&main.join(".claude/settings.json")).contains("procly guard"));

    eject(&procly, &main);

    assert_eq!(read(&main.join(".claude/settings.json")), own);
}

#[test]
fn eject_stops_the_services_of_every_worktree_of_the_repository() {
    let procly = Procly::new();
    let (main, port, _) = repository(&procly);
    let other = git_worktree_add(&main, "app-other");
    assert_success(&procly.procly_in(&main, &["up", "web"]));
    eventually("web to listen", || listens(port));
    let output = procly.procly_in(&other, &["up", "web"]);
    assert_success(&output);
    let other_port: u16 = stdout(&output)
        .trim_end()
        .rsplit(' ')
        .next()
        .and_then(|port| port.parse().ok())
        .expect("a port");

    let ejected = eject(&procly, &other);

    assert!(!listens(port), "the Service of the Main worktree still listens");
    assert!(!listens(other_port), "the Service of the other Worktree still listens");
    let summary = summary(&ejected);
    assert!(
        summary.contains(&format!("stopped web of {}", main.display())),
        "{summary:?}"
    );
    assert!(
        summary.contains(&format!("stopped web of {}", other.display())),
        "{summary:?}"
    );
    assert_eq!(
        stdout(&procly.procly_in(&main, &["status"])).matches("healthy").count(),
        0
    );
}

/// A `claude` that writes its arguments, one per line, and the directory it ran in to `calls`, then exits with `code`.
/// Returns the directory to put first on the `PATH`, and the file it writes.
fn fake_claude(procly: &Procly, code: i32) -> (PathBuf, PathBuf) {
    let bin = procly.scratch().join("bin");
    let calls = procly.scratch().join("claude-calls");
    write(
        &bin.join("claude"),
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$PWD\" \"$@\" >> '{}'\nexit {code}\n",
            calls.display()
        ),
    );
    fs::set_permissions(bin.join("claude"), fs::Permissions::from_mode(0o755)).expect("make it executable");
    (bin, calls)
}

fn eject_with_path(procly: &Procly, dir: &Path, bin: &Path, args: &[&str]) -> Output {
    let path = std::env::join_paths(
        std::iter::once(bin.to_owned()).chain(std::env::split_paths(&std::env::var_os("PATH").expect("PATH is set"))),
    )
    .expect("join the PATH");
    procly
        .command()
        .current_dir(dir)
        .env("PATH", path)
        .arg("eject")
        .args(args)
        .output()
        .expect("procly runs")
}

/// A Repository adopted by Procly whose Rules block was then edited.
fn repository_with_an_edited_rules_block(procly: &Procly) -> PathBuf {
    let (main, _, _) = repository(procly);
    adopt_procly(procly, &main);
    let edited = read(&main.join("AGENTS.md")).replace("Never start", "Please avoid starting");
    write(&main.join("AGENTS.md"), &edited);
    main
}

#[test]
fn no_agent_runs_unless_the_flag_is_given() {
    let procly = Procly::new();
    let main = repository_with_an_edited_rules_block(&procly);
    let (bin, calls) = fake_claude(&procly, 0);

    let output = eject_with_path(&procly, &main, &bin, &[]);

    assert_success(&output);
    assert!(!calls.exists(), "an agent ran without --agent");
    assert!(stdout(&output).contains("the Rules block"), "{}", stdout(&output));
}

#[test]
fn the_agent_gets_the_prompt_in_its_non_interactive_mode_from_the_directory_eject_ran_in() {
    let procly = Procly::new();
    let main = repository_with_an_edited_rules_block(&procly);
    let (bin, calls) = fake_claude(&procly, 0);

    let output = eject_with_path(&procly, &main, &bin, &["--agent", "claude"]);

    assert_success(&output);
    let calls = read(&calls);
    let (directory, rest) = calls.split_once('\n').expect("the agent wrote its directory");
    assert_eq!(Path::new(directory).canonicalize().expect("canonicalize"), main);
    let (flag, prompt) = rest.split_once('\n').expect("the agent wrote its arguments");
    assert_eq!(flag, "-p");
    assert!(
        prompt.contains(&format!("{}: the Rules block", main.join("AGENTS.md").display())),
        "{prompt}"
    );
    // The prompt went to the agent, it is not printed as well.
    assert!(!stdout(&output).contains("the Rules block"), "{}", stdout(&output));
    assert!(summary(&output).iter().any(|line| line.starts_with("left rules ")));
}

#[test]
fn the_agent_is_not_run_when_nothing_was_left_to_undo() {
    let procly = Procly::new();
    let (main, _, _) = repository(&procly);
    adopt_procly(&procly, &main);
    let (bin, calls) = fake_claude(&procly, 0);

    let output = eject_with_path(&procly, &main, &bin, &["--agent", "claude"]);

    assert_success(&output);
    assert!(!calls.exists());
    assert!(stderr(&output).contains("nothing was left"), "{}", stderr(&output));
}

#[test]
fn an_agent_that_fails_or_is_missing_exits_125_and_what_eject_removed_stays_removed() {
    let procly = Procly::new();
    let main = repository_with_an_edited_rules_block(&procly);
    let (bin, _) = fake_claude(&procly, 3);

    let failing = eject_with_path(&procly, &main, &bin, &["--agent", "claude"]);
    assert_eq!(failing.status.code(), Some(125), "{}", stderr(&failing));
    assert!(stderr(&failing).contains("claude exited with"), "{}", stderr(&failing));
    assert!(!main.join(".claude").exists());

    let missing = eject_with_path(&procly, &main, &procly.scratch().join("no-bin"), &["--agent", "codex"]);
    assert_eq!(missing.status.code(), Some(125), "{}", stderr(&missing));
    assert!(stderr(&missing).contains("cannot run codex"), "{}", stderr(&missing));
}

#[test]
fn an_unknown_agent_is_a_usage_error() {
    let procly = Procly::new();
    let (main, _, _) = repository(&procly);

    let output = procly.procly_in(&main, &["eject", "--agent", "emacs"]);

    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
}
