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

/// A Bun and Turborepo style monorepo: 2 apps with a dev script, a library without one, and a root dev command.
const MONOREPO: [(&str, &str); 5] = [
    (
        "package.json",
        r#"{"name": "acme", "workspaces": ["apps/*", "packages/*"], "scripts": {"dev": "turbo dev"}}"#,
    ),
    ("bun.lock", ""),
    (
        "apps/web/package.json",
        r#"{"name": "@acme/web", "scripts": {"dev": "next dev"}}"#,
    ),
    (
        "apps/api/package.json",
        r#"{"name": "api", "scripts": {"dev": "node ace serve"}}"#,
    ),
    (
        "packages/ui/package.json",
        r#"{"name": "@acme/ui", "scripts": {"build": "tsc"}}"#,
    ),
];

#[test]
fn a_monorepo_gets_a_service_per_app_dev_script_and_a_group_for_the_root_dev_command() {
    let procly = Procly::new();
    let main = repository(&procly, &MONOREPO);

    let output = init(&procly, &main);

    let proposal = stdout(&output);
    assert_eq!(
        proposal,
        "\
# Proposed by `procly init`. Review it before you commit it, `procly up` starts what it declares.

[services.api]
run = \"cd apps/api && bun run dev\"

[services.dev]
run = \"bun run dev\"
covers = [\"api\", \"web\"]

[services.web]
run = \"cd apps/web && bun run dev\"
"
    );
    for service in ["web", "api", "dev"] {
        assert_loads(&procly, &main, service);
    }
}

#[test]
fn the_workspaces_of_a_pnpm_monorepo_are_read_from_pnpm_workspace_yaml() {
    let procly = Procly::new();
    let main = repository(
        &procly,
        &[
            (
                "package.json",
                r#"{"name": "acme", "scripts": {"dev": "pnpm -r --parallel dev"}}"#,
            ),
            ("pnpm-workspace.yaml", "packages:\n  - 'apps/*'\n  - '!apps/legacy'\n"),
            (
                "apps/web/package.json",
                r#"{"name": "web", "scripts": {"dev": "vite"}}"#,
            ),
            (
                "apps/legacy/package.json",
                r#"{"name": "legacy", "scripts": {"dev": "vite"}}"#,
            ),
        ],
    );

    let output = init(&procly, &main);

    let proposal = stdout(&output);
    assert!(proposal.contains("run = \"cd apps/web && npm run dev\""), "{proposal}");
    assert!(proposal.contains("covers = [\"web\"]"), "{proposal}");
    assert!(
        !proposal.contains("legacy"),
        "an excluded package is left out\n{proposal}"
    );
}

#[test]
fn a_monorepo_without_a_root_dev_command_gets_its_apps_and_no_group() {
    let procly = Procly::new();
    let main = repository(
        &procly,
        &[
            ("package.json", r#"{"name": "acme", "workspaces": ["apps/*"]}"#),
            (
                "apps/web/package.json",
                r#"{"name": "web", "scripts": {"dev": "vite"}}"#,
            ),
        ],
    );

    let proposal = stdout(&init(&procly, &main));

    assert!(proposal.contains("[services.web]"), "{proposal}");
    assert!(!proposal.contains("covers"), "{proposal}");
}

#[test]
fn an_app_called_dev_does_not_clash_with_the_group() {
    let procly = Procly::new();
    let main = repository(
        &procly,
        &[
            (
                "package.json",
                r#"{"name": "acme", "workspaces": ["apps/*"], "scripts": {"dev": "turbo dev"}}"#,
            ),
            (
                "apps/dev/package.json",
                r#"{"name": "dev", "scripts": {"dev": "vite"}}"#,
            ),
        ],
    );

    init(&procly, &main);

    assert_loads(&procly, &main, "dev");
    assert_loads(&procly, &main, "dev-2");
}

/// The `run` and the declared `http` port of a Service in a proposal.
fn run_and_port(proposal: &str, service: &str) -> (String, Option<i64>) {
    let table: toml::Table = proposal.parse().expect("the proposal is TOML");
    let service = &table["services"][service];
    (
        service["run"].as_str().expect("the Service has a run").to_owned(),
        service.get("ports").and_then(|ports| ports["http"].as_integer()),
    )
}

#[test]
fn a_port_hardcoded_in_a_dev_script_is_declared_and_replaced_by_its_placeholder() {
    let procly = Procly::new();
    let main = repository(
        &procly,
        &[
            ("package.json", r#"{"name": "acme", "workspaces": ["apps/*"]}"#),
            (
                "apps/web/package.json",
                r#"{"name": "web", "scripts": {"dev": "next dev --port 3001"}}"#,
            ),
            (
                "apps/site/package.json",
                r#"{"name": "site", "scripts": {"dev": "vite --port=5173 --host"}}"#,
            ),
            (
                "apps/docs/package.json",
                r#"{"name": "docs", "scripts": {"dev": "serve -p 4000 ."}}"#,
            ),
            (
                "apps/api/package.json",
                r#"{"name": "api", "scripts": {"dev": "PORT=3333 node server.js"}}"#,
            ),
            (
                "apps/mail/package.json",
                r#"{"name": "mail", "scripts": {"dev": "cross-env PORT=\"1080\" mailer"}}"#,
            ),
        ],
    );

    let proposal = stdout(&init(&procly, &main));

    let bins = |up: &str| format!("export PATH=\"$PWD/node_modules/.bin:$PWD/{up}node_modules/.bin:$PATH\"");
    let cases = [
        (
            "web",
            format!("cd apps/web && {} && next dev --port {{ports.http}}", bins("../../")),
            3001,
        ),
        (
            "site",
            format!(
                "cd apps/site && {} && vite --port={{ports.http}} --host",
                bins("../../")
            ),
            5173,
        ),
        (
            "docs",
            format!("cd apps/docs && {} && serve -p {{ports.http}} .", bins("../../")),
            4000,
        ),
        (
            "api",
            format!(
                "cd apps/api && {} && PORT={{ports.http}} node server.js",
                bins("../../")
            ),
            3333,
        ),
        (
            "mail",
            format!(
                "cd apps/mail && {} && cross-env PORT=\"{{ports.http}}\" mailer",
                bins("../../")
            ),
            1080,
        ),
    ];
    for (service, run, port) in cases {
        assert_eq!(
            run_and_port(&proposal, service),
            (run, Some(port)),
            "{service}\n{proposal}"
        );
        assert_loads(&procly, &main, service);
    }
}

#[test]
fn a_dev_script_naming_several_ports_or_none_keeps_running_through_the_package_manager() {
    let procly = Procly::new();
    let main = repository(
        &procly,
        &[
            ("package.json", r#"{"name": "acme", "workspaces": ["apps/*"]}"#),
            (
                "apps/two/package.json",
                r#"{"name": "two", "scripts": {"dev": "a --port 3000 & b --port 3001"}}"#,
            ),
            (
                "apps/none/package.json",
                r#"{"name": "none", "scripts": {"dev": "node server.js"}}"#,
            ),
            (
                "apps/low/package.json",
                r#"{"name": "low", "scripts": {"dev": "jest -p 2"}}"#,
            ),
        ],
    );

    let proposal = stdout(&init(&procly, &main));

    for (service, dir) in [("two", "two"), ("none", "none"), ("low", "low")] {
        assert_eq!(
            run_and_port(&proposal, service),
            (format!("cd apps/{dir} && npm run dev"), None),
            "{proposal}"
        );
    }
}

#[test]
fn a_hardcoded_port_in_the_dev_script_of_the_root_package_is_replaced_too() {
    let procly = Procly::new();
    let main = repository(
        &procly,
        &[(
            "package.json",
            r#"{"name": "shop", "scripts": {"dev": "vite --port 5173"}}"#,
        )],
    );

    let proposal = stdout(&init(&procly, &main));

    assert_eq!(
        run_and_port(&proposal, "shop"),
        (
            "export PATH=\"$PWD/node_modules/.bin:$PATH\" && vite --port {ports.http}".to_owned(),
            Some(5173)
        )
    );
    assert_loads(&procly, &main, "shop");
}
