//! `procly agent install`, `off`, `on` and the Claude Code Guard: agents are steered to Procly without editing
//! anything behind the developer's back (ADR 0008).

mod common;

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

use common::{Procly, assert_success, git_repository, git_worktree_add, stderr, stdout};

const RULES_OPEN: &str = "<!-- >>> procly managed block: rules >>> -->";
const RULES_CLOSE: &str = "<!-- <<< procly managed block: rules <<< -->";

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().expect("the path has a parent")).expect("create the parent");
    fs::write(path, text).expect("write the file");
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).expect("read the file")
}

/// A Repository whose Main worktree holds an `AGENTS.md` the developer wrote.
fn repository_with_instructions(procly: &Procly) -> PathBuf {
    let main = git_repository(procly.scratch(), "app");
    write(&main.join("AGENTS.md"), "# App\n\nBuild with make.\n");
    main
}

/// The text between the markers of the Rules block, empty when the file holds none.
fn rules_block(text: &str) -> &str {
    let Some((_, after)) = text.split_once(RULES_OPEN) else {
        return "";
    };
    after.split_once(RULES_CLOSE).map_or("", |(block, _)| block)
}

#[test]
fn the_rules_block_is_written_only_by_the_explicit_command_and_a_second_run_changes_nothing() {
    let procly = Procly::new();
    let main = repository_with_instructions(&procly);
    let before = read(&main.join("AGENTS.md"));

    // Nothing else Procly does touches an instruction file.
    assert_success(&procly.procly_in(&main, &["status"]));
    assert_eq!(read(&main.join("AGENTS.md")), before);

    let output = procly.procly_in(&main, &["agent", "install"]);

    assert_success(&output);
    let text = read(&main.join("AGENTS.md"));
    assert!(
        text.starts_with(&before),
        "the developer's own text comes first: {text}"
    );
    assert!(rules_block(&text).contains("`procly up <service>`"), "{text}");
    assert!(stdout(&output).contains("rules"), "{}", stdout(&output));

    let second = procly.procly_in(&main, &["agent", "install"]);

    assert_success(&second);
    assert_eq!(read(&main.join("AGENTS.md")), text);
    assert!(stdout(&second).contains("unchanged"), "{}", stdout(&second));
}

#[test]
fn an_instruction_file_is_found_by_name_and_created_when_there_is_none() {
    let procly = Procly::new();
    let only_claude = git_repository(procly.scratch(), "claude-only");
    write(&only_claude.join("CLAUDE.md"), "# Claude only\n");
    let neither = git_repository(procly.scratch(), "neither");
    let both = git_repository(procly.scratch(), "both");
    write(&both.join("AGENTS.md"), "# Agents\n");
    write(&both.join("CLAUDE.md"), "@AGENTS.md\n");

    for repository in [&only_claude, &neither, &both] {
        assert_success(&procly.procly_in(repository, &["agent", "install"]));
    }

    assert!(read(&only_claude.join("CLAUDE.md")).contains(RULES_OPEN));
    assert!(!only_claude.join("AGENTS.md").exists());
    assert!(read(&neither.join("AGENTS.md")).starts_with(RULES_OPEN));
    assert!(read(&both.join("AGENTS.md")).contains(RULES_OPEN));
    assert_eq!(read(&both.join("CLAUDE.md")), "@AGENTS.md\n");
}

#[test]
fn a_rules_block_the_developer_edited_is_left_untouched() {
    let procly = Procly::new();
    let main = repository_with_instructions(&procly);
    assert_success(&procly.procly_in(&main, &["agent", "install"]));
    let edited = read(&main.join("AGENTS.md")).replace("Never start", "Please avoid starting");
    write(&main.join("AGENTS.md"), &edited);

    let output = procly.procly_in(&main, &["agent", "install"]);

    assert_success(&output);
    assert_eq!(read(&main.join("AGENTS.md")), edited);
    assert!(stdout(&output).contains("edited"), "{}", stdout(&output));
    assert!(
        stderr(&output).contains("was edited, left as it is"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn no_instruction_file_outside_the_worktree_is_ever_written() {
    let procly = Procly::new();
    let main = repository_with_instructions(&procly);
    let home = procly.scratch().join("home");
    let global = home.join(".claude/CLAUDE.md");
    write(&global, "my global rules\n");
    let linked = main.join("linked.md");
    std::os::unix::fs::symlink(&global, &linked).expect("link the global file");

    for file in [global.to_str().unwrap(), "../home/.claude/CLAUDE.md", "linked.md"] {
        let output = procly
            .command()
            .current_dir(&main)
            .env("HOME", &home)
            .args(["agent", "install", "--file", file])
            .output()
            .expect("procly runs");
        assert_eq!(output.status.code(), Some(125), "{file}: {}", stderr(&output));
        assert!(
            stderr(&output).contains("outside the Worktree"),
            "{file}: {}",
            stderr(&output)
        );
    }

    assert_eq!(read(&global), "my global rules\n");
    assert_eq!(read(&main.join("AGENTS.md")), "# App\n\nBuild with make.\n");
    // A plain install never looks at the home directory either.
    let output = procly
        .command()
        .current_dir(&main)
        .env("HOME", &home)
        .args(["agent", "install"])
        .output()
        .expect("procly runs");
    assert_success(&output);
    assert_eq!(read(&global), "my global rules\n");
}

#[test]
fn a_default_file_or_settings_directory_that_links_out_of_the_worktree_is_refused() {
    let procly = Procly::new();
    let home = procly.scratch().join("home");
    let global = home.join(".claude/CLAUDE.md");
    write(&global, "my global rules\n");
    let linked_file = git_repository(procly.scratch(), "linked-file");
    std::os::unix::fs::symlink(&global, linked_file.join("AGENTS.md")).expect("link the global file");
    let linked_dir = git_repository(procly.scratch(), "linked-dir");
    std::os::unix::fs::symlink(home.join(".claude"), linked_dir.join(".claude")).expect("link the global directory");

    for repository in [&linked_file, &linked_dir] {
        let output = procly.procly_in(repository, &["agent", "install"]);
        assert_eq!(output.status.code(), Some(125), "{}", stderr(&output));
        assert!(stderr(&output).contains("outside the Worktree"), "{}", stderr(&output));
    }

    assert_eq!(read(&global), "my global rules\n");
    assert!(!home.join(".claude/settings.json").exists());
}

#[test]
fn a_named_file_inside_the_worktree_takes_the_rules_block() {
    let procly = Procly::new();
    let main = repository_with_instructions(&procly);

    let output = procly.procly_in(&main, &["agent", "install", "--file", "docs/agents.md"]);

    assert_success(&output);
    assert!(read(&main.join("docs/agents.md")).contains(RULES_OPEN));
    assert_eq!(read(&main.join("AGENTS.md")), "# App\n\nBuild with make.\n");
}

fn settings(dir: &Path) -> serde_json::Value {
    serde_json::from_str(&read(&dir.join(".claude/settings.json"))).expect("the settings are JSON")
}

#[test]
fn the_guard_is_registered_once_next_to_the_hooks_the_developer_already_has() {
    let procly = Procly::new();
    let main = repository_with_instructions(&procly);
    write(
        &main.join(".claude/settings.json"),
        r#"{"model":"opus","hooks":{"PreToolUse":[{"matcher":"Edit","hooks":[{"type":"command","command":"lint"}]}]}}"#,
    );

    assert_success(&procly.procly_in(&main, &["agent", "install"]));
    let first = read(&main.join(".claude/settings.json"));
    assert_success(&procly.procly_in(&main, &["agent", "install"]));

    assert_eq!(read(&main.join(".claude/settings.json")), first);
    let groups = settings(&main)["hooks"]["PreToolUse"].clone();
    assert_eq!(groups.as_array().unwrap().len(), 2, "{groups}");
    assert_eq!(groups[0]["hooks"][0]["command"], "lint");
    assert_eq!(groups[1]["matcher"], "Bash");
    assert_eq!(groups[1]["hooks"][0]["command"], "procly guard");
    assert_eq!(settings(&main)["model"], "opus");
}

#[test]
fn a_settings_file_that_is_not_json_stops_the_install_before_anything_is_written() {
    let procly = Procly::new();
    let main = repository_with_instructions(&procly);
    write(&main.join(".claude/settings.json"), "{ not json");

    let output = procly.procly_in(&main, &["agent", "install"]);

    assert_eq!(output.status.code(), Some(125));
    assert!(stderr(&output).contains("settings.json"), "{}", stderr(&output));
    assert_eq!(read(&main.join(".claude/settings.json")), "{ not json");
    assert_eq!(read(&main.join("AGENTS.md")), "# App\n\nBuild with make.\n");
}

/// Runs `procly guard` from `dir` the way Claude Code does before a tool call, with the call on its standard input.
fn guard_input(procly: &Procly, dir: &Path, input: &serde_json::Value) -> Output {
    let mut child = procly
        .command()
        .current_dir(dir)
        .arg("guard")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("procly guard runs");
    child
        .stdin
        .take()
        .expect("stdin is piped")
        .write_all(input.to_string().as_bytes())
        .expect("write the tool call");
    child.wait_with_output().expect("procly guard ends")
}

/// A Bash tool call of `command` made from `dir`.
fn guard(procly: &Procly, dir: &Path, command: &str) -> Output {
    guard_input(
        procly,
        dir,
        &serde_json::json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
            "tool_input": { "command": command },
            "cwd": dir,
        }),
    )
}

/// The reason the Guard gave for denying the call, `None` when it let it through.
fn denial(output: &Output) -> Option<String> {
    assert_success(output);
    let text = stdout(output);
    if text.trim().is_empty() {
        return None;
    }
    let answer: serde_json::Value = serde_json::from_str(&text).expect("the Guard answers in JSON");
    assert_eq!(answer["hookSpecificOutput"]["hookEventName"], "PreToolUse");
    assert_eq!(answer["hookSpecificOutput"]["permissionDecision"], "deny");
    Some(
        answer["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .expect("a reason")
            .to_owned(),
    )
}

/// A Repository declaring the Services `web` and `api`.
fn repository_with_services(procly: &Procly) -> PathBuf {
    let main = repository_with_instructions(procly);
    write(
        &main.join("procly.toml"),
        "[services.web]\nrun = 'npm run dev'\n[services.api]\nrun = 'node api.js'\n",
    );
    main
}

#[test]
fn the_guard_blocks_a_known_dev_server_command_and_answers_with_the_procly_command() {
    let procly = Procly::new();
    let main = repository_with_services(&procly);

    for (command, instead) in [
        // The command of a declared Service, or the start of it.
        ("npm run dev", "`procly up web`"),
        ("npm run dev -- --host", "`procly up web`"),
        // A dev command of the whole Repository, run at its root.
        ("yarn start", "`procly up`"),
        ("bash -c 'bun run dev'", "`procly up`"),
        // Nothing declared starts it, the agent picks the Service.
        ("pnpm --filter web dev", "`procly up <service>`"),
        ("bun run dev:web", "`procly up <service>`"),
        ("next dev -p 3001", "`procly up <service>`"),
        ("cd apps/web && PORT=3000 npx vite", "`procly up <service>`"),
        ("cd apps/web && npm start", "`procly up <service>`"),
        ("node ace serve --hmr", "`procly up <service>`"),
        ("docker compose up -d", "`procly up <service>`"),
        ("docker-compose -f compose.dev.yml up db", "`procly up <service>`"),
    ] {
        let reason = denial(&guard(&procly, &main, command)).unwrap_or_else(|| panic!("{command} went through"));
        assert!(reason.contains(&format!("Run {instead}")), "{command}: {reason}");
        assert!(
            reason.contains("web, api") || reason.contains("api, web"),
            "{command}: {reason}"
        );
    }
}

#[test]
fn the_guard_answers_a_compose_command_with_the_compose_services_it_starts() {
    let procly = Procly::new();
    let main = repository_with_instructions(&procly);
    write(
        &main.join("procly.toml"),
        "[services.db]\ncompose = 'postgres'\n[services.cache]\ncompose = 'redis'\n[services.web]\nrun = 'npm run dev'\n",
    );

    for (command, instead) in [
        ("docker compose up -d postgres", "`procly up db`"),
        ("docker-compose -f compose.dev.yml up redis", "`procly up cache`"),
        ("docker compose up -d", "`procly up cache && procly up db`"),
        ("docker compose up mailpit", "`procly up <service>`"),
    ] {
        let reason = denial(&guard(&procly, &main, command)).unwrap_or_else(|| panic!("{command} went through"));
        assert!(reason.contains(&format!("Run {instead}")), "{command}: {reason}");
    }
}

#[test]
fn the_guard_sends_a_root_dev_command_run_below_the_root_to_the_service_to_pick() {
    let procly = Procly::new();
    let main = repository_with_services(&procly);
    let below = main.join("apps/web");
    std::fs::create_dir_all(&below).unwrap();

    let reason = denial(&guard(&procly, &below, "yarn start")).expect("blocked");

    assert!(reason.contains("Run `procly up <service>`"), "{reason}");
}

#[test]
fn the_guard_names_the_command_it_blocked() {
    let procly = Procly::new();
    let main = repository_with_services(&procly);

    let reason = denial(&guard(&procly, &main, "cd apps/web && npm run dev")).expect("blocked");

    assert!(reason.starts_with("`npm run dev`"), "{reason}");
}

#[test]
fn the_guard_lets_unrelated_commands_through_without_starting_the_daemon() {
    let procly = Procly::new();
    let main = repository_with_services(&procly);

    for command in [
        "ls -la",
        "npm test",
        "npm run build",
        "pnpm install",
        "git commit -m 'npm run dev'",
        "echo 'docker compose up'",
        "cat package.json | grep dev",
        "docker compose ps",
        "docker compose run --rm web npm test",
        "cargo test",
        "vite build",
        "curl http://localhost:3000/dev",
        "",
    ] {
        assert_eq!(denial(&guard(&procly, &main, command)), None, "{command}");
    }

    assert!(
        !procly.state_dir().join("procly.sock").exists(),
        "a command the Guard does not know must not start the Daemon"
    );
}

#[test]
fn the_guard_lets_through_what_it_cannot_judge() {
    let procly = Procly::new();
    let main = repository_with_services(&procly);

    // Another tool, a call without a command, and a input that is not JSON at all.
    let other_tool =
        serde_json::json!({ "tool_name": "Edit", "tool_input": { "command": "npm run dev" }, "cwd": main });
    assert_eq!(denial(&guard_input(&procly, &main, &other_tool)), None);
    let no_command = serde_json::json!({ "tool_name": "Bash", "tool_input": {}, "cwd": main });
    assert_eq!(denial(&guard_input(&procly, &main, &no_command)), None);
    let garbage = guard_input(&procly, &main, &serde_json::json!("not a tool call"));
    assert_eq!(denial(&garbage), None);

    // A directory outside any Repository has no Services for Procly to start, so the command is not blocked.
    let outside = procly.scratch().join("elsewhere");
    fs::create_dir_all(&outside).expect("create the directory");
    assert_eq!(denial(&guard(&procly, &outside, "npm run dev")), None);
}

#[test]
fn the_guard_answers_in_a_repository_that_declares_no_service() {
    let procly = Procly::new();
    let main = repository_with_instructions(&procly);

    let reason = denial(&guard(&procly, &main, "npm run dev")).expect("blocked");

    assert!(reason.contains("No Service is declared"), "{reason}");
}

#[test]
fn off_makes_the_guard_and_the_rules_block_stand_aside_and_on_restores_them() {
    let procly = Procly::new();
    let main = repository_with_services(&procly);
    let other = git_worktree_add(&main, "app-other");
    assert_success(&procly.procly_in(&main, &["agent", "install"]));
    let on_rules = read(&main.join("AGENTS.md"));
    assert!(denial(&guard(&procly, &main, "npm run dev")).is_some());

    let off = procly.procly_in(&other, &["off"]);

    assert_success(&off);
    assert!(stdout(&off).starts_with("off"), "{}", stdout(&off));
    // The whole Repository stands aside, from any of its Worktrees, and across a restart of the Daemon.
    procly.stop_daemon();
    assert_eq!(denial(&guard(&procly, &main, "npm run dev")), None);
    assert_eq!(denial(&guard(&procly, &other, "docker compose up")), None);
    let off_rules = read(&main.join("AGENTS.md"));
    assert!(rules_block(&off_rules).contains("Procly is off"), "{off_rules}");
    assert!(!rules_block(&off_rules).contains("procly up <service>"), "{off_rules}");
    assert!(off_rules.starts_with("# App\n\nBuild with make.\n"), "{off_rules}");
    // Nothing was removed: the Guard stays registered.
    assert_eq!(
        settings(&main)["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
        "procly guard"
    );

    let on = procly.procly_in(&main, &["on"]);

    assert_success(&on);
    assert!(stdout(&on).starts_with("on"), "{}", stdout(&on));
    assert_eq!(read(&main.join("AGENTS.md")), on_rules);
    assert!(denial(&guard(&procly, &other, "npm run dev")).is_some());
}

#[test]
fn off_leaves_a_rules_block_the_developer_edited_and_other_repositories_alone() {
    let procly = Procly::new();
    let main = repository_with_services(&procly);
    let elsewhere = git_repository(procly.scratch(), "elsewhere");
    write(&elsewhere.join("procly.toml"), "[services.site]\nrun = 'npm run dev'\n");
    assert_success(&procly.procly_in(&main, &["agent", "install"]));
    assert_success(&procly.procly_in(&elsewhere, &["agent", "install"]));
    let edited = read(&main.join("AGENTS.md")).replace("Never start", "Please avoid starting");
    write(&main.join("AGENTS.md"), &edited);
    let untouched = read(&elsewhere.join("AGENTS.md"));

    let off = procly.procly_in(&main, &["off"]);

    assert_success(&off);
    assert_eq!(read(&main.join("AGENTS.md")), edited);
    assert!(stderr(&off).contains("was edited, left as it is"), "{}", stderr(&off));
    assert_eq!(read(&elsewhere.join("AGENTS.md")), untouched);
    assert_eq!(denial(&guard(&procly, &main, "npm run dev")), None);
    assert!(denial(&guard(&procly, &elsewhere, "npm run dev")).is_some());
}

#[test]
fn the_rules_block_installed_while_off_says_so() {
    let procly = Procly::new();
    let main = repository_with_services(&procly);
    assert_success(&procly.procly_in(&main, &["off"]));

    let install = procly.procly_in(&main, &["agent", "install"]);

    assert_success(&install);
    assert!(rules_block(&read(&main.join("AGENTS.md"))).contains("Procly is off"));
    assert!(stderr(&install).contains("off"), "{}", stderr(&install));
    assert_success(&procly.procly_in(&main, &["on"]));
    assert!(rules_block(&read(&main.join("AGENTS.md"))).contains("`procly up <service>`"));
}
