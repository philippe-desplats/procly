//! `procly eject`: everything Procly wrote into a Repository is removed, except the Managed blocks the developer
//! edited, which a generated prompt describes (ADR 0008).

mod common;

use std::fs;
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Output;

use common::{
    Procly, assert_success, eventually, free_port, git, git_output, git_repository, git_worktree_add, json_document,
    json_schema_version, json_succeeded, stderr, stdout, test_server,
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
    // A Pool of its own, so that a second Worktree never races the other tests for a port of the Range.
    let pool = free_port();
    write(&main.join("AGENTS.md"), INSTRUCTIONS);
    write(
        &main.join("procly.toml"),
        &format!(
            "[services.web]\nrun = '{} --port {{ports.http}}'\nports = {{ http = {{ port = {port}, pool = [{pool}] }} }}\n",
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

/// Ejects the whole Repository, as `procly eject --repository` does from any of its Worktrees.
fn eject_repository(procly: &Procly, dir: &Path) -> Output {
    let output = procly.procly_in(dir, &["eject", "--repository"]);
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
    let (main, _, _) = repository(&procly);
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
}

#[test]
fn an_env_file_the_developer_added_a_line_to_is_left_and_described() {
    let procly = Procly::new();
    let (main, _, _) = repository(&procly);
    adopt_procly(&procly, &main);
    let edited = format!("{}MY_OWN=1\n", read(&main.join(".env.procly")));
    write(&main.join(".env.procly"), &edited);

    let output = eject(&procly, &main);

    assert_eq!(read(&main.join(".env.procly")), edited);
    assert!(
        summary(&output).contains(&format!("left env file {}, edited", main.join(".env.procly").display())),
        "{}",
        stdout(&output)
    );
}

#[test]
fn a_second_eject_finds_nothing_to_remove() {
    let procly = Procly::new();
    let (main, _, _) = repository(&procly);
    adopt_procly(&procly, &main);
    eject(&procly, &main);

    let output = eject(&procly, &main);

    assert_eq!(
        stdout(&output),
        "left Docker volumes and Logical resources\nleft the files copied by worktree init and the state of the Daemon, Leases and off\n"
    );
    assert_eq!(stderr(&output), "");
}

#[test]
fn eject_with_repository_cleans_every_worktree_and_every_file_procly_wrote_in() {
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

    let output = eject_repository(&procly, &other);

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
fn eject_with_repository_stops_the_services_of_every_worktree_of_the_repository() {
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

    let ejected = eject_repository(&procly, &other);

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

/// The port a `procly up` printed, its last word.
fn port_of(output: &Output) -> u16 {
    stdout(output)
        .trim_end()
        .rsplit(' ')
        .next()
        .and_then(|port| port.parse().ok())
        .expect("a port")
}

/// A Repository whose Main worktree and a secondary Worktree both took Procly in and run `web`, with the ports they
/// listen on.
struct Both {
    main: PathBuf,
    main_port: u16,
    other: PathBuf,
    other_port: u16,
    before: Before,
}

fn procly_in_both_worktrees(procly: &Procly) -> Both {
    let (main, main_port, before) = repository(procly);
    let other = git_worktree_add(&main, "app-other");
    adopt_procly(procly, &main);
    assert_success(&procly.procly_in(&other, &["agent", "install"]));
    let output = procly.procly_in(&other, &["up", "web"]);
    assert_success(&output);
    let other_port = port_of(&output);
    eventually("web of the Main worktree to listen", || listens(main_port));
    eventually("web of the other Worktree to listen", || listens(other_port));
    Both {
        main,
        main_port,
        other,
        other_port,
        before,
    }
}

#[test]
fn eject_from_a_secondary_worktree_leaves_the_main_worktree_running_and_its_files_in_place() {
    let procly = Procly::new();
    let both = procly_in_both_worktrees(&procly);
    let main_env = read(&both.main.join(".env.procly"));
    let main_rules = read(&both.main.join("AGENTS.md"));
    let main_settings = read(&both.main.join(".claude/settings.json"));
    let exclude = read(&both.main.join(".git/info/exclude"));
    assert_ne!(exclude, both.before.exclude, "the exclusions block was written");

    let output = eject(&procly, &both.other);

    assert!(listens(both.main_port), "the Service of the Main worktree was stopped");
    assert!(
        !listens(both.other_port),
        "the Service of the current Worktree still listens"
    );
    assert_eq!(read(&both.main.join(".env.procly")), main_env);
    assert_eq!(read(&both.main.join(".git/info/exclude")), exclude);
    assert_eq!(read(&both.main.join("AGENTS.md")), main_rules);
    assert_eq!(read(&both.main.join(".claude/settings.json")), main_settings);
    assert!(!both.other.join(".env.procly").exists());
    assert!(!both.other.join(".claude").exists());
    assert!(!read(&both.other.join("AGENTS.md")).contains(RULES_OPEN));
    let summary = summary(&output);
    assert!(
        summary.contains(&format!("stopped web of {}", both.other.display())),
        "{summary:?}"
    );
    let main = both.main.display().to_string();
    assert!(
        !summary
            .iter()
            .any(|line| line.starts_with("stopped") && line.ends_with(&main)),
        "{summary:?}"
    );
    assert!(
        !summary
            .iter()
            .any(|line| line.starts_with("removed") && line.contains(&format!("{main}/"))),
        "{summary:?}"
    );
}

#[test]
fn eject_says_what_it_left_of_the_repository_and_of_the_other_worktrees_and_why() {
    let procly = Procly::new();
    let both = procly_in_both_worktrees(&procly);

    let output = eject(&procly, &both.other);

    let summary = summary(&output);
    let main = both.main.display();
    for line in [
        format!("kept web of {main}, it runs for another Worktree"),
        format!("kept rules {main}/AGENTS.md, written for another Worktree"),
        format!("kept guard {main}/.claude/settings.json, written for another Worktree"),
        format!("kept env file {main}/.env.procly, written for another Worktree"),
        format!("kept exclusions {main}/.git/info/exclude, shared by every Worktree of the Repository"),
    ] {
        assert!(summary.contains(&line), "{line:?} is not in {summary:?}");
    }
}

#[test]
fn eject_of_a_repository_with_one_worktree_removes_the_shared_exclusions_too() {
    let procly = Procly::new();
    let (main, _, before) = repository(&procly);
    adopt_procly(&procly, &main);

    let output = eject(&procly, &main);

    assert_eq!(read(&main.join(".git/info/exclude")), before.exclude);
    assert!(!summary(&output).iter().any(|line| line.starts_with("kept ")));
}

#[test]
fn eject_from_the_main_worktree_leaves_a_secondary_worktree_alone_too() {
    let procly = Procly::new();
    let both = procly_in_both_worktrees(&procly);

    eject(&procly, &both.main);

    assert!(!listens(both.main_port));
    assert!(
        listens(both.other_port),
        "the Service of the other Worktree was stopped"
    );
    assert!(both.other.join(".env.procly").exists());
    assert!(both.other.join(".claude/settings.json").exists());
    assert!(read(&both.main.join(".git/info/exclude")).contains("procly managed block: exclusions"));
}

#[test]
fn eject_with_repository_acts_on_every_worktree_and_lists_the_services_of_the_others_it_stops() {
    let procly = Procly::new();
    let both = procly_in_both_worktrees(&procly);

    let output = procly.procly_in(&both.other, &["eject", "--repository"]);

    assert_success(&output);
    assert!(!listens(both.main_port));
    assert!(!listens(both.other_port));
    assert!(!both.main.join(".env.procly").exists());
    assert!(!both.other.join(".env.procly").exists());
    assert_eq!(read(&both.main.join(".git/info/exclude")), both.before.exclude);
    let summary = summary(&output);
    for worktree in [&both.main, &both.other] {
        assert!(
            summary.contains(&format!("stopped web of {}", worktree.display())),
            "{summary:?}"
        );
    }
    assert!(!summary.iter().any(|line| line.starts_with("kept ")), "{summary:?}");
}

#[test]
fn eject_stops_the_jobs_of_the_current_worktree_only() {
    let procly = Procly::new();
    let both = procly_in_both_worktrees(&procly);
    let started = |dir: &Path| {
        let output = procly.procly_in(dir, &["run", "sleep 30"]);
        assert_success(&output);
        common::started(&output).0
    };
    let (main_job, other_job) = (started(&both.main), started(&both.other));

    let output = eject(&procly, &both.other);

    let summary = summary(&output);
    assert!(
        summary.contains(&format!(
            "stopped job {other_job} of {}, sleep 30",
            both.other.display()
        )),
        "{summary:?}"
    );
    assert!(
        !summary.iter().any(|line| line.contains(&main_job)),
        "the Job of the Main worktree was stopped: {summary:?}"
    );
    let status = stdout(&procly.procly_in(&both.main, &["status"]));
    assert!(
        status.contains(&main_job) && status.contains("running"),
        "the Job of the Main worktree is not running: {status}"
    );
}

#[test]
fn eject_with_repository_stops_the_jobs_of_every_worktree() {
    let procly = Procly::new();
    let both = procly_in_both_worktrees(&procly);
    let jobs: Vec<_> = [&both.main, &both.other]
        .into_iter()
        .map(|dir| {
            let output = procly.procly_in(dir, &["run", "sleep 30"]);
            assert_success(&output);
            common::started(&output).0
        })
        .collect();

    let output = eject_repository(&procly, &both.other);

    let summary = summary(&output);
    for (job, dir) in jobs.iter().zip([&both.main, &both.other]) {
        assert!(
            summary.contains(&format!("stopped job {job} of {}, sleep 30", dir.display())),
            "{summary:?}"
        );
    }
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

/// A Repository whose `procly.toml` and `files` are committed as they are, byte for byte, before Procly sees them.
fn repository_with_files(procly: &Procly, files: &[(&str, &str)]) -> (PathBuf, u16) {
    let main = git_repository(procly.scratch(), "app");
    let port = free_port();
    write(
        &main.join("procly.toml"),
        &format!(
            "[services.web]\nrun = '{} --port {{ports.http}}'\nports = {{ http = {port} }}\n",
            test_server().display()
        ),
    );
    for (name, text) in files {
        write(&main.join(name), text);
    }
    git(&main, &["add", "."]);
    git(&main, &["commit", "--quiet", "--message", "app"]);
    (main, port)
}

#[test]
fn an_instruction_file_without_a_final_line_break_comes_back_without_one() {
    let procly = Procly::new();
    let (main, _) = repository_with_files(&procly, &[("AGENTS.md", "# App\n\nBuild with make.")]);
    adopt_procly(&procly, &main);

    eject(&procly, &main);

    assert_eq!(
        fs::read(main.join("AGENTS.md")).expect("read"),
        b"# App\n\nBuild with make."
    );
    assert_eq!(git_output(&main, &["status", "--porcelain"]), "");
}

#[test]
fn a_tracked_empty_instruction_file_and_settings_file_stay_empty_files() {
    let procly = Procly::new();
    let (main, _) = repository_with_files(&procly, &[("AGENTS.md", ""), (".claude/settings.json", "")]);
    adopt_procly(&procly, &main);
    assert!(read(&main.join("AGENTS.md")).contains(RULES_OPEN));

    let output = eject(&procly, &main);

    for file in ["AGENTS.md", ".claude/settings.json"] {
        assert_eq!(
            fs::read(main.join(file)).expect("the file is still there"),
            b"",
            "{file}"
        );
    }
    assert_eq!(git_output(&main, &["status", "--porcelain"]), "");
    assert!(!stdout(&output).contains("was deleted"), "{}", stdout(&output));
}

#[test]
fn a_settings_file_indented_with_4_spaces_comes_back_byte_for_byte() {
    let procly = Procly::new();
    let own = "{\n    \"model\": \"opus\",\n    \"hooks\": {\n        \"PreToolUse\": [\n            {\n                \"matcher\": \"Edit\",\n                \"hooks\": [\n                    {\n                        \"type\": \"command\",\n                        \"command\": \"lint\"\n                    }\n                ]\n            }\n        ]\n    }\n}\n";
    let (main, _) = repository_with_files(&procly, &[("AGENTS.md", INSTRUCTIONS), (".claude/settings.json", own)]);
    adopt_procly(&procly, &main);
    assert!(read(&main.join(".claude/settings.json")).contains("procly guard"));

    eject(&procly, &main);

    assert_eq!(read(&main.join(".claude/settings.json")), own);
    assert_eq!(git_output(&main, &["status", "--porcelain"]), "");
}

#[test]
fn eject_json_carries_what_was_stopped_removed_and_left_and_the_prompt() {
    let procly = Procly::new();
    let main = repository_with_an_edited_rules_block(&procly);

    let output = eject_with_path(&procly, &main, &procly.scratch().join("no-bin"), &["--json"]);

    let document = json_succeeded(&output);
    assert_eq!(document["schema_version"], json_schema_version(&procly));
    assert_eq!(
        document["stopped"],
        serde_json::json!([{ "service": "web", "worktree": main }])
    );
    let removed = |kind: &str| {
        document["removed"]
            .as_array()
            .expect("a list of removed items")
            .iter()
            .any(|item| item["kind"] == kind)
    };
    assert!(
        removed("guard") && removed("exclusions") && removed("env_file"),
        "{document}"
    );
    assert!(!removed("rules"), "{document}");
    assert_eq!(
        document["left"],
        serde_json::json!([{ "kind": "rules", "file": main.join("AGENTS.md") }])
    );
    let prompt = document["prompt"].as_str().expect("the prompt of what was left");
    assert!(
        prompt.contains(&format!("{}: the Rules block", main.join("AGENTS.md").display())),
        "{prompt}"
    );
    assert_eq!(document["agent"], serde_json::json!(null));
    assert_eq!(document["jobs"], serde_json::json!([]));
    assert_eq!(document["spared"], serde_json::json!([]));
}

#[test]
fn eject_json_names_the_jobs_stopped_and_what_it_spared_with_its_kind() {
    let procly = Procly::new();
    let both = procly_in_both_worktrees(&procly);
    let output = procly.procly_in(&both.other, &["run", "sleep 30"]);
    assert_success(&output);
    let job = common::started(&output).0;

    let output = procly.procly_in(&both.other, &["eject", "--json"]);

    let document = json_succeeded(&output);
    assert_eq!(
        document["jobs"],
        serde_json::json!([{ "job": job, "command": "sleep 30", "worktree": both.other }])
    );
    let spared = document["spared"].as_array().expect("a list of spared items");
    assert!(
        spared.contains(&serde_json::json!({ "kind": "service", "service": "web", "worktree": both.main })),
        "{document}"
    );
    assert!(
        spared.contains(&serde_json::json!({
            "kind": "file",
            "written": "exclusions",
            "file": both.main.join(".git/info/exclude"),
            "shared": true
        })),
        "{document}"
    );
    assert!(
        spared.contains(&serde_json::json!({
            "kind": "file",
            "written": "env_file",
            "file": both.main.join(".env.procly"),
            "shared": false
        })),
        "{document}"
    );
}

#[test]
fn eject_json_with_an_agent_keeps_stdout_to_the_document_and_names_the_agent() {
    let procly = Procly::new();
    let main = repository_with_an_edited_rules_block(&procly);
    let bin = procly.scratch().join("bin");
    write(&bin.join("claude"), "#!/bin/sh\necho the agent talks\n");
    fs::set_permissions(bin.join("claude"), fs::Permissions::from_mode(0o755)).expect("make it executable");

    let output = eject_with_path(&procly, &main, &bin, &["--json", "--agent", "claude"]);

    assert_success(&output);
    let document = json_document(&output);
    assert_eq!(document["agent"], serde_json::json!("claude"));
    assert_eq!(document["prompt"], serde_json::json!(null));
    assert_eq!(document["left"][0]["kind"], serde_json::json!("rules"));
    assert!(stderr(&output).contains("the agent talks"), "{}", stderr(&output));
}
