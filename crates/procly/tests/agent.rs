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
fn the_guard_goes_into_a_tab_indented_settings_file_without_reindenting_it() {
    let procly = Procly::new();
    let main = repository_with_instructions(&procly);
    write(
        &main.join(".claude/settings.json"),
        "{\n\t\"model\": \"opus\",\n\t\"env\": {\n\t\t\"A\": \"1\"\n\t}\n}\n",
    );

    assert_success(&procly.procly_in(&main, &["agent", "install"]));

    let written = read(&main.join(".claude/settings.json"));
    let kept =
        "{\n\t\"model\": \"opus\",\n\t\"env\": {\n\t\t\"A\": \"1\"\n\t},\n\t\"hooks\": {\n\t\t\"PreToolUse\": [\n";
    assert!(written.starts_with(kept), "{written:?}");
    assert!(written.ends_with("\t}\n}\n"), "{written:?}");
    assert!(written.lines().all(|line| !line.starts_with(' ')), "{written:?}");
    assert_eq!(
        settings(&main)["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
        "procly guard"
    );
}

#[test]
fn the_guard_goes_into_a_four_space_settings_file_without_reindenting_it() {
    let procly = Procly::new();
    let main = repository_with_instructions(&procly);
    write(&main.join(".claude/settings.json"), "{\n    \"model\": \"opus\"\n}\n");

    assert_success(&procly.procly_in(&main, &["agent", "install"]));

    let written = read(&main.join(".claude/settings.json"));
    let kept = "{\n    \"model\": \"opus\",\n    \"hooks\": {\n        \"PreToolUse\": [\n";
    assert!(written.starts_with(kept), "{written:?}");
    assert!(written.ends_with("    }\n}\n"), "{written:?}");
    assert_eq!(
        settings(&main)["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
        "procly guard"
    );
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
        ("docker-compose -f compose.yaml up db", "`procly up <service>`"),
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
        ("docker-compose -f compose.yaml up redis", "`procly up cache`"),
        ("docker compose up -d", "`procly up cache && procly up db`"),
        ("docker compose up mailpit", "`procly up <service>`"),
    ] {
        let reason = denial(&guard(&procly, &main, command)).unwrap_or_else(|| panic!("{command} went through"));
        assert!(reason.contains(&format!("Run {instead}")), "{command}: {reason}");
    }
}

#[test]
fn the_guard_judges_a_compose_file_given_with_an_absolute_path_against_the_root_of_the_worktree() {
    let procly = Procly::new();
    let main = repository_with_instructions(&procly);
    write(
        &main.join("procly.toml"),
        "[services.db]\ncompose = 'postgres'\n[services.cache]\ncompose = 'redis'\n",
    );
    let below = main.join("apps/web");
    std::fs::create_dir_all(&below).unwrap();
    let root = main.canonicalize().unwrap();
    let root = root.display();

    // Any absolute path but the compose file of the root, inside the Worktree or not, is not managed by Procly.
    for (dir, file) in [
        (&main, format!("{root}/infra/other.yml")),
        (&below, format!("{root}/infra/other.yml")),
        (&main, format!("{root}/compose.dev.yml")),
        (&main, "/srv/app/compose.yaml".to_owned()),
        (&main, format!("{root}/../compose.yaml")),
    ] {
        let command = format!("docker compose -f {file} up -d");
        let reason = denial(&guard(&procly, dir, &command)).unwrap_or_else(|| panic!("{command} went through"));
        assert!(
            reason.contains(&format!("The compose file `{file}` is not managed by Procly"))
                && !reason.contains("procly up")
                && !reason.contains("Declared Services"),
            "{command}: {reason}"
        );
    }

    // The compose file of the root, named by its absolute path, is the one Procly drives.
    for (dir, file) in [
        (&main, format!("{root}/compose.yaml")),
        (&below, format!("{root}/compose.yaml")),
        (&main, format!("{root}/apps/../compose.yaml")),
    ] {
        let command = format!("docker compose -f {file} up -d");
        let reason = denial(&guard(&procly, dir, &command)).unwrap_or_else(|| panic!("{command} went through"));
        assert!(
            reason.contains("Run `procly up cache && procly up db`"),
            "{command}: {reason}"
        );
    }
}

#[test]
fn the_guard_says_a_compose_file_procly_does_not_manage_is_not_and_names_no_service() {
    let procly = Procly::new();
    let main = repository_with_instructions(&procly);
    write(
        &main.join("procly.toml"),
        "[services.db]\ncompose = 'postgres'\n[services.cache]\ncompose = 'redis'\n",
    );
    let below = main.join("apps/web");
    std::fs::create_dir_all(&below).unwrap();

    for (dir, command, file) in [
        (&main, "docker compose -f compose.dev.yml up -d", "compose.dev.yml"),
        (
            &main,
            "docker-compose --file infra/compose.yaml up redis",
            "infra/compose.yaml",
        ),
        (&below, "docker compose -f compose.yaml up -d", "compose.yaml"),
        (
            &main,
            "cd apps/web && docker compose -f compose.yaml up -d",
            "compose.yaml",
        ),
    ] {
        let reason = denial(&guard(&procly, dir, command)).unwrap_or_else(|| panic!("{command} went through"));
        assert!(
            reason.contains(&format!("The compose file `{file}` is not managed by Procly"))
                && !reason.contains("procly up"),
            "{command}: {reason}"
        );
    }

    // The compose file of the root, named or not, from the root or from below it, is the one Procly drives.
    for (dir, command) in [
        (&main, "docker compose -f ./compose.yaml up -d"),
        (&below, "docker compose -f ../../compose.yaml up -d"),
        (&below, "docker compose up -d"),
    ] {
        let reason = denial(&guard(&procly, dir, command)).unwrap_or_else(|| panic!("{command} went through"));
        assert!(
            reason.contains("Run `procly up cache && procly up db`"),
            "{command}: {reason}"
        );
    }
}

#[test]
fn the_guard_names_the_service_that_runs_in_the_directory_of_a_dev_command() {
    let procly = Procly::new();
    let main = repository_with_instructions(&procly);
    write(
        &main.join("procly.toml"),
        "[services.web]\nrun = 'cd apps/web && bun run dev'\n[services.api]\nrun = 'cd apps/api && bun run dev'\n",
    );
    let web = main.join("apps/web");
    let docs = main.join("apps/docs");
    std::fs::create_dir_all(&web).unwrap();
    std::fs::create_dir_all(&docs).unwrap();

    for (dir, command, instead) in [
        (&web, "bun dev", "`procly up web`"),
        (&main, "cd apps/api && bun dev", "`procly up api`"),
        (&web, "cd ../api && bun dev", "`procly up api`"),
        (&docs, "bun dev", "`procly up <service>`"),
    ] {
        let reason = denial(&guard(&procly, dir, command)).unwrap_or_else(|| panic!("{command} went through"));
        assert!(reason.contains(&format!("Run {instead}")), "{command}: {reason}");
    }
}

#[test]
fn the_guard_moves_a_dev_command_by_its_directory_option_and_by_an_absolute_cd() {
    let procly = Procly::new();
    let main = repository_with_instructions(&procly);
    write(
        &main.join("procly.toml"),
        "[services.web]\nrun = 'cd apps/web && bun run dev'\n[services.api]\nrun = 'cd apps/api && bun run dev'\n\
         [services.dev]\nrun = 'bun run dev'\ncovers = ['web', 'api']\n",
    );
    std::fs::create_dir_all(main.join("apps/web")).unwrap();
    std::fs::create_dir_all(main.join("apps/api")).unwrap();
    let absolute = format!("cd {}/apps/web && bun run dev", main.display());
    // The root as the agent may write it, through another path than the one the Daemon knows, as `/var` for
    // `/private/var`.
    let link = main.with_file_name("link-to-main");
    std::os::unix::fs::symlink(&main, &link).unwrap();
    let through_link = format!("cd {}/apps/web && bun run dev", link.display());

    for (command, instead) in [
        ("bun --cwd apps/web run dev", "`procly up web`"),
        ("bun run --cwd apps/web dev", "`procly up web`"),
        (absolute.as_str(), "`procly up web`"),
        (through_link.as_str(), "`procly up web`"),
        // Moved to apps/web, but web runs `bun` and not `yarn`, so the agent picks the Service.
        ("yarn --cwd apps/web dev", "`procly up <service>`"),
        // Narrowed to one package, so not the Group of the whole Repository.
        ("turbo dev --filter=@acme/web", "`procly up <service>`"),
    ] {
        let reason = denial(&guard(&procly, &main, command)).unwrap_or_else(|| panic!("{command} went through"));
        assert!(
            reason.contains(&format!("Run {instead} instead.")),
            "{command}: {reason}"
        );
        assert!(
            ["web", "api", "dev"].iter().all(|name| reason.contains(name)),
            "{command}: {reason}"
        );
    }
}

/// The reason the Guard gives for `command` run from `dir`, with the command itself left out, so that a command and
/// the same command behind a wrapper can be compared by what the Guard tells the agent to run.
fn instead(procly: &Procly, dir: &Path, command: &str) -> String {
    let reason = denial(&guard(procly, dir, command)).unwrap_or_else(|| panic!("{command} went through"));
    let quoted = reason.find("` ").map_or(0, |end| end + 2);
    reason[quoted..].to_owned()
}

#[test]
fn the_guard_answers_a_dev_command_behind_a_wrapper_as_it_answers_it_without() {
    // The Group `dev` at the root runs the script its root `package.json` runs, here with turbo, there with bun.
    for (group_run, commands) in [
        (
            "turbo run dev",
            &[
                (
                    "",
                    "cross-env FORCE_COLOR=1 turbo run dev --parallel",
                    "FORCE_COLOR=1 turbo run dev --parallel",
                    "`procly up dev`",
                ),
                (
                    "",
                    "npx cross-env FORCE_COLOR=1 turbo run dev",
                    "FORCE_COLOR=1 turbo run dev",
                    "`procly up dev`",
                ),
                ("apps/web", "dotenv -e .env -- next dev", "next dev", "`procly up web`"),
                (
                    "apps/web",
                    "NODE_EXTRA_CA_CERTS=./ca.pem dotenv -e .env -- next dev --experimental-https",
                    "NODE_EXTRA_CA_CERTS=./ca.pem next dev --experimental-https",
                    "`procly up web`",
                ),
                // A `--` further on belongs to the command behind the wrapper.
                (
                    "apps/web",
                    "dotenv -e .env npm run dev -- --port 3001",
                    "npm run dev -- --port 3001",
                    "`procly up <service>`",
                ),
                (
                    "apps/web",
                    "env-cmd -f .env pnpm dev -- --port 3001",
                    "pnpm dev -- --port 3001",
                    "`procly up <service>`",
                ),
                (
                    "apps/web",
                    "dotenvx run -f .env next dev -- --turbo",
                    "next dev -- --turbo",
                    "`procly up web`",
                ),
                (
                    "apps/web",
                    "dotenvx run --convention nextjs -- next dev",
                    "next dev",
                    "`procly up web`",
                ),
                // The command line `cross-env-shell` hands to a shell, by path and after a `cd`.
                (
                    "apps/web",
                    "./node_modules/.bin/cross-env-shell NODE_ENV=dev \"next dev\"",
                    "next dev",
                    "`procly up web`",
                ),
                (
                    "",
                    "cd apps/web && cross-env-shell NODE_ENV=dev \"next dev\"",
                    "cd apps/web && next dev",
                    "`procly up web`",
                ),
            ][..],
        ),
        (
            "bun run dev",
            &[(
                "",
                "cross-env PORT=3000 bun run dev",
                "PORT=3000 bun run dev",
                "`procly up dev`",
            )][..],
        ),
    ] {
        let procly = Procly::new();
        let main = repository_with_instructions(&procly);
        write(
            &main.join("procly.toml"),
            &format!(
                "[services.web]\nrun = 'cd apps/web && next dev'\n[services.api]\nrun = 'cd apps/api && bun run dev'\n\
                 [services.dev]\nrun = '{group_run}'\ncovers = ['web', 'api']\n"
            ),
        );
        std::fs::create_dir_all(main.join("apps/web")).unwrap();
        std::fs::create_dir_all(main.join("apps/api")).unwrap();

        for (dir, wrapped, unwrapped, expected) in commands {
            let dir = main.join(dir);
            let without = instead(&procly, &dir, unwrapped);
            assert!(
                without.contains(&format!("Run {expected} instead.")),
                "{unwrapped}: {without}"
            );
            assert_eq!(instead(&procly, &dir, wrapped), without, "{wrapped}");
        }

        // A wrapper in front of a command that starts no dev server hides nothing either.
        for command in [
            "cross-env NODE_ENV=production bun run build",
            "dotenv -e .env -- bun run build",
            "dotenv -e .env npm run build -- next dev",
        ] {
            assert_eq!(denial(&guard(&procly, &main, command)), None, "{command}");
        }
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

/// The line of the Rules block naming the declared Services, `None` when the block has none.
fn services_line(text: &str) -> Option<&str> {
    rules_block(text)
        .lines()
        .find(|line| line.starts_with("- The services of this repository"))
}

/// A Repository whose `procly.toml` declares Services out of alphabetical order and a Group covering 2 of them.
fn repository_with_a_group(procly: &Procly) -> PathBuf {
    let main = repository_with_instructions(procly);
    write(
        &main.join("procly.toml"),
        "[services.worker]\nrun = 'node worker.js'\n\
         [services.dev]\nrun = 'turbo dev'\ncovers = ['web', 'api']\n\
         [services.web]\nrun = 'npm run dev'\n\
         [services.api]\nrun = 'node api.js'\n",
    );
    main
}

#[test]
fn the_rules_block_names_the_declared_services_and_groups_in_the_order_of_the_file() {
    let procly = Procly::new();
    let main = repository_with_a_group(&procly);

    assert_success(&procly.procly_in(&main, &["agent", "install"]));

    assert_eq!(
        services_line(&read(&main.join("AGENTS.md"))),
        Some(
            "- The services of this repository: `worker`, `web`, `api`, and the Group `dev` covering `web` and `api`."
        )
    );
}

#[test]
fn the_rules_block_names_only_what_the_versioned_file_declares_never_the_local_one() {
    let procly = Procly::new();
    let main = repository_with_a_group(&procly);
    // A personal file, excluded from git, that declares a Service of its own and widens the Group.
    write(
        &main.join("procly.local.toml"),
        "[services.mine]\nrun = 'node mine.js'\n[services.dev]\ncovers = ['web', 'api', 'mine']\n",
    );
    let versioned = Some(
        "- The services of this repository: `worker`, `web`, `api`, and the Group `dev` covering `web` and `api`.",
    );

    assert_success(&procly.procly_in(&main, &["agent", "install"]));
    assert_eq!(services_line(&read(&main.join("AGENTS.md"))), versioned);

    assert_success(&procly.procly_in(&main, &["off"]));
    assert_success(&procly.procly_in(&main, &["on"]));
    assert_eq!(services_line(&read(&main.join("AGENTS.md"))), versioned);
}

#[test]
fn the_rules_block_of_a_repository_with_no_declared_service_names_none() {
    let procly = Procly::new();
    let main = repository_with_instructions(&procly);

    assert_success(&procly.procly_in(&main, &["agent", "install"]));

    let text = read(&main.join("AGENTS.md"));
    assert!(rules_block(&text).contains("`procly up <service>`"), "{text}");
    assert_eq!(services_line(&text), None, "{text}");
}

#[test]
fn the_off_rules_block_names_no_service_and_on_names_them_again() {
    let procly = Procly::new();
    let main = repository_with_a_group(&procly);
    assert_success(&procly.procly_in(&main, &["agent", "install"]));
    let on_rules = read(&main.join("AGENTS.md"));

    assert_success(&procly.procly_in(&main, &["off"]));

    let off_rules = read(&main.join("AGENTS.md"));
    assert_eq!(
        rules_block(&off_rules),
        "\n## Procly (off)\n\nProcly is off for this repository. Start dev servers and containers with their usual \
         commands, and do not run `procly up`.\n\nThe developer turns it back on with `procly on`.\n"
    );
    assert_success(&procly.procly_in(&main, &["on"]));
    assert_eq!(read(&main.join("AGENTS.md")), on_rules);
    assert!(services_line(&on_rules).is_some(), "{on_rules}");
}

#[test]
fn the_rules_block_starts_after_a_blank_line_whatever_the_end_of_the_file() {
    for (before, separator) in [
        ("# App\n\nBuild with make.\n", "\n"),
        ("# App\n\nBuild with make.", "\n\n"),
        ("# App\n\nBuild with make.\n\n", ""),
    ] {
        let procly = Procly::new();
        let main = git_repository(procly.scratch(), "app");
        write(&main.join("CLAUDE.md"), before);

        assert_success(&procly.procly_in(&main, &["agent", "install"]));

        let text = read(&main.join("CLAUDE.md"));
        assert!(
            text.starts_with(&format!("{before}{separator}{RULES_OPEN}\n")),
            "{before:?} became {text:?}"
        );
        // A second install leaves the file as it is, with no blank line added on each run.
        assert_success(&procly.procly_in(&main, &["agent", "install"]));
        assert_eq!(read(&main.join("CLAUDE.md")), text);
    }
}

#[test]
fn a_rules_block_in_an_empty_or_absent_file_starts_on_its_first_line() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    write(&main.join("CLAUDE.md"), "");
    let other = git_worktree_add(&main, "app-other");

    assert_success(&procly.procly_in(&main, &["agent", "install"]));
    assert_success(&procly.procly_in(&other, &["agent", "install"]));

    assert!(read(&main.join("CLAUDE.md")).starts_with(RULES_OPEN));
    assert!(read(&other.join("AGENTS.md")).starts_with(RULES_OPEN));
}
