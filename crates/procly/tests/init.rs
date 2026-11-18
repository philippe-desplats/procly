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

# The Group dev isolates a secondary Worktree only when each app it covers reads its port from PORT_<SERVICE>_<NAME>, otherwise start the Services one by one.
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
fn a_dev_script_serving_https_gets_a_port_named_https_and_its_group_reads_port_service_https() {
    let procly = Procly::new();
    let main = repository(
        &procly,
        &[
            (
                "package.json",
                r#"{"name": "acme", "workspaces": ["apps/*"], "scripts": {"dev": "turbo dev"}}"#,
            ),
            (
                "apps/web/package.json",
                r#"{"name": "web", "scripts": {"dev": "next dev --experimental-https --port 3001"}}"#,
            ),
            (
                "apps/site/package.json",
                r#"{"name": "site", "scripts": {"dev": "vite --https --port=5173"}}"#,
            ),
            (
                "apps/api/package.json",
                r#"{"name": "api", "scripts": {"dev": "next dev --port 3333"}}"#,
            ),
        ],
    );

    let proposal = stdout(&init(&procly, &main));

    let table: toml::Table = proposal.parse().expect("the proposal is TOML");
    let service = |name: &str| table["services"][name].clone();
    let bins = "export PATH=\"$PWD/node_modules/.bin:$PWD/../../node_modules/.bin:$PATH\"";
    assert_eq!(
        service("web")["run"].as_str(),
        Some(format!("cd apps/web && {bins} && next dev --experimental-https --port {{ports.https}}").as_str()),
        "{proposal}"
    );
    assert_eq!(
        service("web")["ports"],
        toml::Value::Table(toml::toml! { https = 3001 }),
        "{proposal}"
    );
    assert_eq!(
        service("site")["run"].as_str(),
        Some(format!("cd apps/site && {bins} && vite --https --port={{ports.https}}").as_str()),
        "{proposal}"
    );
    assert_eq!(
        service("site")["ports"],
        toml::Value::Table(toml::toml! { https = 5173 }),
        "{proposal}"
    );
    // Without the flag, the port stays `http`.
    assert_eq!(
        service("api")["ports"],
        toml::Value::Table(toml::toml! { http = 3333 }),
        "{proposal}"
    );
    let env = procly.procly_in(&main, &["env", "dev"]);
    assert_success(&env);
    let env = stdout(&env);
    for variable in ["PORT_WEB_HTTPS=", "PORT_SITE_HTTPS=", "PORT_API_HTTP="] {
        assert!(env.contains(variable), "{variable} in\n{env}");
    }
    for service in ["web", "site", "api"] {
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

const COMPOSE: &str = r#"
services:
  postgres:
    image: postgres:17
    environment:
      POSTGRES_DB: shop
    ports:
      - "${DB_PORT:-5432}:5432"
  redis:
    image: redis:7
    ports:
      - "6379:6379"
  mailpit:
    image: axllent/mailpit
    ports:
      - "1025:1025"
      - "8025:8025"
"#;

#[test]
fn compose_services_are_proposed_shared_or_logical_with_their_ports() {
    let procly = Procly::new();
    let main = repository(&procly, &[("compose.yaml", COMPOSE)]);

    let output = init(&procly, &main);

    assert_eq!(
        stdout(&output),
        "\
# Proposed by `procly init`. Review it before you commit it, `procly up` starts what it declares.

[services.mailpit]
compose = \"mailpit\"

[services.mailpit.health]
port = \"smtp\"

[services.mailpit.ports]
http = 8025
smtp = 1025

[services.postgres]
compose = \"postgres\"
driver = \"postgres\"

[services.postgres.env]
DB_PORT = \"{services.postgres.ports.db}\"

[services.postgres.ports]
db = 5432

[services.postgres.resources.dev]
name = \"shop\"

[services.redis]
compose = \"redis\"
driver = \"redis\"

[services.redis.ports]
redis = 6379

[services.redis.resources.main]
"
    );
    for service in ["postgres", "redis", "mailpit"] {
        assert_loads(&procly, &main, service);
    }
}

#[test]
fn a_port_the_compose_file_reads_from_a_variable_procly_sets_needs_no_env_and_names_the_port() {
    let procly = Procly::new();
    let main = repository(
        &procly,
        &[(
            "docker-compose.yml",
            "services:\n  db:\n    image: pgvector/pgvector:pg16\n    ports:\n      - '127.0.0.1:${PORT_DB:-5432}:5432'\n",
        )],
    );

    let proposal = stdout(&init(&procly, &main));

    assert!(proposal.contains("[services.db.ports]\ndb = 5432\n"), "{proposal}");
    assert!(!proposal.contains("env"), "{proposal}");
    assert_loads(&procly, &main, "db");
}

#[test]
fn long_mappings_merge_keys_and_mappings_without_a_host_port_are_read() {
    let procly = Procly::new();
    let main = repository(
        &procly,
        &[(
            "compose.yml",
            "\
x-defaults: &defaults
  restart: unless-stopped
services:
  mq:
    <<: *defaults
    image: rabbitmq:3-management
    ports:
      - target: 5672
        published: '${AMQP_PORT:-5672}'
      - target: 15672
        published: 15672
      - 4369
      - '9999/udp'
      - '127.0.0.1:8000-8010:8000'
  worker:
    <<: *defaults
    build: .
",
        )],
    );

    let proposal = stdout(&init(&procly, &main));

    assert!(
        proposal.contains("[services.mq.ports]\namqp = 5672\nhttp = 15672\n"),
        "{proposal}"
    );
    assert!(
        proposal.contains("[services.mq.env]\nAMQP_PORT = \"{services.mq.ports.amqp}\"\n"),
        "{proposal}"
    );
    assert!(
        proposal.contains("[services.mq.health]\nport = \"amqp\"\n"),
        "{proposal}"
    );
    assert!(
        proposal.contains("[services.worker]\ncompose = \"worker\"\n"),
        "{proposal}"
    );
    assert_loads(&procly, &main, "mq");
    assert_loads(&procly, &main, "worker");
}

#[test]
fn a_postgres_that_publishes_no_port_stays_shared_and_a_variable_two_services_read_is_derived_once() {
    let procly = Procly::new();
    let main = repository(
        &procly,
        &[(
            "compose.yaml",
            "\
services:
  internal:
    image: postgres:17
  one:
    image: nginx
    ports: ['${HTTP_PORT:-8080}:80']
  two:
    image: nginx
    ports: ['${HTTP_PORT:-8081}:80']
",
        )],
    );

    let proposal = stdout(&init(&procly, &main));

    assert!(!proposal.contains("driver"), "{proposal}");
    assert_eq!(proposal.matches("HTTP_PORT").count(), 1, "{proposal}");
    for service in ["internal", "one", "two"] {
        assert_loads(&procly, &main, service);
    }
}

const BARE_POSTGRES: &str = "\
services:
  db:
    image: postgres:17
    ports: ['5432:5432']
";

/// The `name` of the `dev` resource of the Postgres service `db` in a proposal.
fn database_name(proposal: &str) -> String {
    let document: toml::Table = proposal.parse().expect("the proposal is TOML");
    document["services"]["db"]["resources"]["dev"]["name"]
        .as_str()
        .expect("the resource is named")
        .to_owned()
}

#[test]
fn a_postgres_that_sets_no_database_is_named_after_the_repository_and_stderr_says_so() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "Acme-Shop");
    write(&main.join("compose.yaml"), BARE_POSTGRES);

    let output = init(&procly, &main);

    assert_eq!(database_name(&stdout(&output)), "acme_shop");
    let note = stderr(&output);
    assert!(note.contains("`acme_shop`") && note.contains("POSTGRES_DB"), "{note}");
    assert_loads(&procly, &main, "db");
}

#[test]
fn a_postgres_that_sets_no_database_in_a_secondary_worktree_is_named_after_the_main_worktree() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "acme-shop");
    write(&main.join("compose.yaml"), BARE_POSTGRES);
    common::git(&main, &["add", "compose.yaml"]);
    common::git(&main, &["commit", "--quiet", "--message", "add the compose file"]);
    let feature = common::git_worktree_add(&main, "feature");

    let output = init(&procly, &feature);

    assert_eq!(database_name(&stdout(&output)), "acme_shop");
}

#[test]
fn a_database_is_never_a_maintenance_database_whatever_the_repository_is_called() {
    for (repository, name) in [
        ("postgres", "postgres_dev"),
        ("Template1", "template1_dev"),
        ("_", "app"),
        (
            "a-repository-name-that-is-far-too-long-for-a-database",
            "a_repository_name_that_is_far",
        ),
    ] {
        let procly = Procly::new();
        let main = git_repository(procly.scratch(), repository);
        write(&main.join("compose.yaml"), BARE_POSTGRES);

        let output = init(&procly, &main);

        assert_eq!(database_name(&stdout(&output)), name, "{repository}");
        assert_loads(&procly, &main, "db");
    }
}

#[test]
fn a_postgres_that_names_a_maintenance_database_is_named_after_the_repository_instead() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "acme-shop");
    write(
        &main.join("compose.yaml"),
        "\
services:
  db:
    image: postgres:17
    environment:
      POSTGRES_DB: template1
    ports: ['5432:5432']
",
    );

    let output = init(&procly, &main);

    assert_eq!(database_name(&stdout(&output)), "acme_shop");
    assert!(stderr(&output).contains("template1"), "{}", stderr(&output));
}

#[test]
fn a_postgres_that_sets_its_database_keeps_that_name_and_stderr_has_no_note() {
    let procly = Procly::new();
    let main = repository(&procly, &[("compose.yaml", COMPOSE)]);

    let output = init(&procly, &main);

    assert!(stdout(&output).contains("name = \"shop\""), "{}", stdout(&output));
    assert_eq!(stderr(&output).lines().count(), 1, "{}", stderr(&output));
}

#[test]
fn a_compose_service_named_like_an_app_gets_a_name_of_its_own_and_keeps_its_compose_name() {
    let procly = Procly::new();
    let main = repository(
        &procly,
        &[
            (
                "package.json",
                r#"{"name": "api", "scripts": {"dev": "node server.js"}}"#,
            ),
            (
                "compose.yaml",
                "services:\n  api:\n    image: nginx\n    ports: ['8080:80']\n",
            ),
        ],
    );

    let proposal = stdout(&init(&procly, &main));

    assert!(
        proposal.contains("[services.api]\nrun = \"npm run dev\"\n"),
        "{proposal}"
    );
    assert!(proposal.contains("[services.api-2]\ncompose = \"api\"\n"), "{proposal}");
    assert_loads(&procly, &main, "api");
    assert_loads(&procly, &main, "api-2");
}

#[test]
fn a_compose_file_that_is_not_yaml_is_refused_naming_the_file_and_nothing_is_written() {
    let procly = Procly::new();
    let main = repository(&procly, &[("compose.yaml", "services: [unclosed\n")]);

    let output = procly.procly_in(&main, &["init"]);

    assert_eq!(output.status.code(), Some(125));
    assert!(stderr(&output).contains("invalid YAML in"), "{}", stderr(&output));
    assert!(stderr(&output).contains("compose.yaml"), "{}", stderr(&output));
    assert!(!main.join("procly.toml").exists());
}

#[test]
fn a_package_json_that_is_not_json_is_refused_naming_the_file() {
    let procly = Procly::new();
    let main = repository(&procly, &[("package.json", "{ not json")]);

    let output = procly.procly_in(&main, &["init"]);

    assert_eq!(output.status.code(), Some(125));
    assert!(stderr(&output).contains("invalid JSON in"), "{}", stderr(&output));
}

#[test]
fn init_run_from_a_subdirectory_or_a_secondary_worktree_writes_at_the_root_of_that_worktree() {
    let procly = Procly::new();
    let main = repository(
        &procly,
        &[(
            "package.json",
            r#"{"name": "shop", "scripts": {"dev": "node server.js"}}"#,
        )],
    );
    common::git(&main, &["add", "package.json"]);
    common::git(&main, &["commit", "--quiet", "--message", "add the package"]);
    let feature = common::git_worktree_add(&main, "feature");
    fs::create_dir(feature.join("deep")).unwrap();

    init(&procly, &feature.join("deep"));

    assert!(feature.join("procly.toml").is_file());
    assert!(!main.join("procly.toml").exists());
}

#[test]
fn a_double_star_workspace_pattern_reaches_packages_at_any_depth() {
    let procly = Procly::new();
    let main = repository(
        &procly,
        &[
            ("package.json", r#"{"name": "acme", "workspaces": ["apps/**"]}"#),
            (
                "apps/web/package.json",
                r#"{"name": "web", "scripts": {"dev": "vite"}}"#,
            ),
            (
                "apps/group/api/package.json",
                r#"{"name": "api", "scripts": {"dev": "node api.js"}}"#,
            ),
            (
                "apps/group/api/node_modules/dep/package.json",
                r#"{"name": "dep", "scripts": {"dev": "x"}}"#,
            ),
        ],
    );

    let proposal = stdout(&init(&procly, &main));

    assert!(proposal.contains("cd apps/web && npm run dev"), "{proposal}");
    assert!(proposal.contains("cd apps/group/api && npm run dev"), "{proposal}");
    assert!(!proposal.contains("dep"), "{proposal}");
}

#[test]
fn a_postgres_with_credentials_the_bundled_driver_does_not_know_stays_shared() {
    let procly = Procly::new();
    let main = repository(
        &procly,
        &[(
            "compose.yaml",
            "\
services:
  db:
    image: postgres:17
    environment:
      POSTGRES_USER: shop
      POSTGRES_PASSWORD: secret
    ports: ['5432:5432']
  plain:
    image: postgres:17
    environment:
      POSTGRES_USER: postgres
      POSTGRES_PASSWORD: postgres
    ports: ['5433:5432']
",
        )],
    );

    let proposal = stdout(&init(&procly, &main));

    assert!(!proposal.contains("[services.db.resources"), "{proposal}");
    assert!(proposal.contains("[services.plain.resources.dev]"), "{proposal}");
    assert_eq!(proposal.matches("driver").count(), 1, "{proposal}");
}

#[test]
fn init_in_a_fresh_state_directory_leaves_no_daemon_running() {
    let procly = Procly::new();
    let main = repository(
        &procly,
        &[(
            "package.json",
            r#"{"name": "shop", "scripts": {"dev": "node server.js"}}"#,
        )],
    );
    let outside = procly.scratch().join("not-a-repository");
    fs::create_dir(&outside).unwrap();

    init(&procly, &main);
    assert_success(&procly.procly_in(&main, &["init", "--json"]));
    assert_eq!(procly.procly_in(&outside, &["init"]).status.code(), Some(125));

    let left: Vec<_> = fs::read_dir(procly.state_dir())
        .expect("read the state directory")
        .map(|entry| entry.expect("read an entry of the state directory").file_name())
        .collect();
    assert!(
        left.is_empty(),
        "init needs no Daemon, yet the state directory holds {left:?}"
    );
}

/// A monorepo of 2 apps whose root `dev` script is `script`.
fn monorepo_with_root_script(procly: &Procly, script: &str) -> PathBuf {
    let root = serde_json::json!({
        "name": "acme",
        "workspaces": ["apps/*"],
        "scripts": { "dev": script },
    });
    repository(
        procly,
        &[
            ("package.json", &root.to_string()),
            (
                "apps/web/package.json",
                r#"{"name": "web", "scripts": {"dev": "next dev"}}"#,
            ),
            (
                "apps/api/package.json",
                r#"{"name": "api", "scripts": {"dev": "node ace serve"}}"#,
            ),
        ],
    )
}

#[test]
fn a_root_dev_script_that_starts_commands_init_did_not_map_yields_a_group_without_them() {
    let procly = Procly::new();
    let main = monorepo_with_root_script(
        &procly,
        "turbo dev & stripe listen --forward-to localhost:3000/api/webhooks & node worker.js",
    );

    let output = init(&procly, &main);

    let proposal = stdout(&output);
    let (run, _) = run_and_port(&proposal, "dev");
    assert_eq!(run, "turbo dev", "{proposal}");
    assert!(proposal.contains(r#"covers = ["api", "web"]"#), "{proposal}");
    let notes = stderr(&output);
    for left_out in [
        "stripe listen --forward-to localhost:3000/api/webhooks",
        "node worker.js",
    ] {
        let note = notes
            .lines()
            .find(|line| line.contains(left_out))
            .unwrap_or_else(|| panic!("stderr names {left_out}\n{notes}"));
        assert!(note.contains("not recognised as a dev server"), "{note}");
    }
    assert_loads(&procly, &main, "dev");
}

/// What `procly init` proposes for the Group `dev` of a monorepo whose root `dev` script is `script`: its run and what
/// it covers, `None` without a Group, and the notes it prints.
fn group_proposal(script: &str) -> (Option<String>, Option<String>, Vec<String>) {
    let procly = Procly::new();
    let main = monorepo_with_root_script(&procly, script);
    let output = init(&procly, &main);
    let table: toml::Table = stdout(&output).parse().expect("the proposal is TOML");
    let group = table["services"].get("dev");
    let notes = stderr(&output)
        .lines()
        .filter(|line| !line.starts_with("procly: wrote"))
        .map(str::to_owned)
        .collect();
    (
        group.and_then(|group| group["run"].as_str()).map(str::to_owned),
        group.and_then(|group| group.get("covers")).map(ToString::to_string),
        notes,
    )
}

#[test]
fn a_root_dev_script_behind_a_wrapper_gets_the_proposal_of_its_unwrapped_form() {
    // A `--` of the command behind the wrapper, and the command line `cross-env-shell` hands to a shell.
    for (wrapped, unwrapped, kept) in [
        (
            "env-cmd -f .env turbo dev -- --concurrency 20 & stripe listen",
            "turbo dev -- --concurrency 20 & stripe listen",
            "env-cmd -f .env turbo dev -- --concurrency 20",
        ),
        (
            r#"cross-env-shell NODE_ENV=dev "cd apps/web && next dev" & stripe listen"#,
            "cd apps/web && next dev & stripe listen",
            r#"cross-env-shell NODE_ENV=dev "cd apps/web && next dev""#,
        ),
        // The line runs from where `cross-env-shell` runs, here `apps`.
        (
            r#"cd apps && cross-env-shell X=1 "cd web && next dev" & stripe listen"#,
            "cd apps && cd web && next dev & stripe listen",
            r#"cd apps && cross-env-shell X=1 "cd web && next dev""#,
        ),
    ] {
        let (run, covers, notes) = group_proposal(unwrapped);
        assert!(covers.is_some(), "{unwrapped}: {run:?} {notes:?}");
        let (wrapped_run, wrapped_covers, wrapped_notes) = group_proposal(wrapped);
        assert_eq!(
            (&wrapped_covers, &wrapped_notes),
            (&covers, &notes),
            "{wrapped}: {wrapped_run:?}, unwrapped {run:?}"
        );
        assert_eq!(wrapped_run.as_deref(), Some(kept), "{wrapped}");
    }
}

#[test]
fn a_root_dev_script_with_no_recognised_command_gets_no_group_and_stderr_says_so() {
    let procly = Procly::new();
    let main = monorepo_with_root_script(&procly, "stripe listen && node worker.js");

    let output = init(&procly, &main);

    let proposal = stdout(&output);
    assert!(!proposal.contains("covers"), "{proposal}");
    assert!(
        proposal.contains("[services.web]") && proposal.contains("[services.api]"),
        "{proposal}"
    );
    let notes = stderr(&output);
    assert!(
        notes.contains("`stripe listen`") && notes.contains("`node worker.js`"),
        "{notes}"
    );
    assert!(notes.contains("No Group is proposed"), "{notes}");
    assert_loads(&procly, &main, "web");
}

#[test]
fn the_commands_concurrently_runs_are_left_out_one_by_one_with_their_names() {
    let procly = Procly::new();
    let main = monorepo_with_root_script(
        &procly,
        r#"concurrently -n web,hooks,api "pnpm --filter web dev" "stripe listen" "pnpm --filter api dev""#,
    );

    let output = init(&procly, &main);

    let (run, _) = run_and_port(&stdout(&output), "dev");
    assert_eq!(
        run,
        r#"concurrently -n web,api "pnpm --filter web dev" "pnpm --filter api dev""#
    );
    assert!(stderr(&output).contains("`stripe listen`"), "{}", stderr(&output));
    assert_loads(&procly, &main, "dev");
}

#[test]
fn a_prep_step_left_out_says_the_server_after_it_no_longer_waits() {
    let procly = Procly::new();
    let main = monorepo_with_root_script(&procly, "pnpm build && turbo dev");

    let output = init(&procly, &main);

    let (run, _) = run_and_port(&stdout(&output), "dev");
    assert_eq!(run, "turbo dev");
    let notes = stderr(&output);
    assert!(
        notes.contains("`pnpm build`") && notes.contains("`turbo dev` used to run after it"),
        "{notes}"
    );
}

#[test]
fn a_root_dev_script_of_recognised_commands_only_keeps_the_package_manager_and_prints_no_note() {
    let procly = Procly::new();
    let main = monorepo_with_root_script(&procly, "next dev & vite");

    let output = init(&procly, &main);

    let (run, _) = run_and_port(&stdout(&output), "dev");
    assert_eq!(run, "npm run dev");
    assert!(!stderr(&output).contains("left out"), "{}", stderr(&output));
}

/// The `covers` of the Group `dev` of `proposal`, as the lines of TOML it is written with.
fn covers(proposal: &str) -> String {
    proposal
        .lines()
        .find(|line| line.starts_with("covers"))
        .unwrap_or_else(|| panic!("no Group covers anything\n{proposal}"))
        .to_owned()
}

#[test]
fn a_group_narrowed_to_one_package_covers_only_its_service() {
    let procly = Procly::new();
    let main = monorepo_with_root_script(
        &procly,
        r#"concurrently "pnpm --filter web dev" "stripe listen --forward-to localhost:3000/api/webhooks" "node worker.js""#,
    );

    let output = init(&procly, &main);

    let proposal = stdout(&output);
    let (run, _) = run_and_port(&proposal, "dev");
    assert_eq!(run, r#"concurrently "pnpm --filter web dev""#, "{proposal}");
    assert_eq!(covers(&proposal), r#"covers = ["web"]"#, "{proposal}");
    assert!(
        proposal.contains("[services.api]"),
        "the Service the Group does not start stays declared\n{proposal}"
    );
    assert!(stderr(&output).contains("`stripe listen"), "{}", stderr(&output));
    assert_loads(&procly, &main, "dev");
    assert_loads(&procly, &main, "api");
}

#[test]
fn a_prep_step_left_out_does_not_widen_what_the_narrowed_group_covers() {
    let procly = Procly::new();
    let main = monorepo_with_root_script(&procly, "pnpm build && pnpm --filter web dev");

    let output = init(&procly, &main);

    let proposal = stdout(&output);
    let (run, _) = run_and_port(&proposal, "dev");
    assert_eq!(run, "pnpm --filter web dev", "{proposal}");
    assert_eq!(covers(&proposal), r#"covers = ["web"]"#, "{proposal}");
    assert_loads(&procly, &main, "dev");
}

#[test]
fn a_command_run_from_the_directory_of_a_package_covers_that_service() {
    let procly = Procly::new();
    let main = monorepo_with_root_script(&procly, "cd apps/api && node ace serve & stripe listen");

    let output = init(&procly, &main);

    let proposal = stdout(&output);
    let (run, _) = run_and_port(&proposal, "dev");
    assert_eq!(run, "cd apps/api && node ace serve", "{proposal}");
    assert_eq!(covers(&proposal), r#"covers = ["api"]"#, "{proposal}");
}

#[test]
fn a_kept_command_that_names_no_package_gets_no_group_and_stderr_says_so() {
    let procly = Procly::new();
    let main = monorepo_with_root_script(&procly, "next dev & stripe listen");

    let output = init(&procly, &main);

    let proposal = stdout(&output);
    assert!(!proposal.contains("covers"), "{proposal}");
    assert!(
        proposal.contains("[services.web]") && proposal.contains("[services.api]"),
        "{proposal}"
    );
    let notes = stderr(&output);
    assert!(
        notes.contains("`next dev` cannot be tied to one Service") && notes.contains("No Group is proposed"),
        "{notes}"
    );
    assert_loads(&procly, &main, "web");
}

#[test]
fn a_command_run_after_a_cd_home_is_said_to_run_outside_the_worktree() {
    for script in [
        "cd && next dev & stripe listen",
        "cd ~/tools && next dev & stripe listen",
    ] {
        let procly = Procly::new();
        let main = monorepo_with_root_script(&procly, script);

        let output = init(&procly, &main);

        let proposal = stdout(&output);
        assert!(!proposal.contains("covers"), "{script}: {proposal}");
        let notes = stderr(&output);
        assert!(
            notes.contains("it runs from a directory outside the Worktree") && !notes.contains("holds no Service"),
            "{script}: {notes}"
        );
    }
}

#[test]
fn init_suggests_worktree_hooks_when_the_worktree_has_a_worktrunk_config() {
    let procly = Procly::new();
    let main = repository(
        &procly,
        &[
            (
                "package.json",
                r#"{"name": "shop", "scripts": {"dev": "node server.js"}}"#,
            ),
            (".config/wt.toml", "post-create = \"npm ci\"\n"),
        ],
    );

    let output = init(&procly, &main);

    let notes = stderr(&output);
    assert!(
        notes.contains(".config/wt.toml") && notes.contains("`procly worktree hooks`"),
        "{notes}"
    );
    assert_eq!(
        fs::read_to_string(main.join(".config/wt.toml")).expect("read the Worktrunk config"),
        "post-create = \"npm ci\"\n",
        "init only suggests, it writes nothing into the Worktrunk config"
    );
}

#[test]
fn init_says_nothing_of_worktree_hooks_without_a_worktrunk_config_or_once_the_block_is_there() {
    let procly = Procly::new();
    let scripts = (
        "package.json",
        r#"{"name": "shop", "scripts": {"dev": "node server.js"}}"#,
    );
    let without = repository(&procly, &[scripts]);

    assert!(!stderr(&init(&procly, &without)).contains("worktree hooks"));

    let procly = Procly::new();
    let with_block = repository(&procly, &[scripts]);
    assert_success(&procly.procly_in(&with_block, &["worktree", "hooks"]));

    assert!(!stderr(&init(&procly, &with_block)).contains("worktree hooks"));
}

/// What stderr and the comment above a proposed Group say about secondary Worktrees, before any fixed port is named.
const GROUP_WARNING: &str = "the Group dev isolates a secondary Worktree only when each app it covers reads its port \
                             from PORT_<SERVICE>_<NAME>, otherwise start the Services one by one";

/// The line of stderr that warns about the Group, which there is exactly one of.
fn group_warning(output: &Output) -> String {
    let notes = stderr(output);
    let warnings: Vec<&str> = notes
        .lines()
        .filter(|line| line.contains("isolates a secondary"))
        .collect();
    assert_eq!(warnings.len(), 1, "one line warns about the Group\n{notes}");
    warnings[0].to_owned()
}

#[test]
fn a_proposed_group_is_said_to_isolate_secondary_worktrees_only_through_the_port_variables() {
    let procly = Procly::new();
    let main = repository(&procly, &MONOREPO);

    let output = init(&procly, &main);

    assert_eq!(group_warning(&output), format!("procly: {GROUP_WARNING}."));
    let proposal = stdout(&output);
    let mut lines = proposal.lines().skip_while(|line| !line.starts_with("# The Group dev"));
    assert!(
        lines
            .next()
            .is_some_and(|comment| comment.contains("PORT_<SERVICE>_<NAME>")),
        "{proposal}"
    );
    assert_eq!(
        lines.next(),
        Some("[services.dev]"),
        "the comment sits right above the Group\n{proposal}"
    );
    assert_loads(&procly, &main, "dev");
}

#[test]
fn no_group_proposed_means_no_warning_about_it() {
    let procly = Procly::new();
    let main = repository(
        &procly,
        &[
            ("package.json", r#"{"name": "acme", "workspaces": ["apps/*"]}"#),
            (
                "apps/web/package.json",
                r#"{"name": "web", "scripts": {"dev": "next dev"}}"#,
            ),
        ],
    );

    let output = init(&procly, &main);

    assert!(!stderr(&output).contains("isolates a secondary"), "{}", stderr(&output));
    assert!(!stdout(&output).contains("isolates a secondary"), "{}", stdout(&output));
}

#[test]
fn a_port_in_the_env_example_of_a_covered_app_is_named_with_its_file() {
    let procly = Procly::new();
    let mut files = MONOREPO.to_vec();
    files.push((
        "apps/web/.env.example",
        "# copy to .env\nDATABASE_URL=postgres://localhost/web\nPORT=3001\n",
    ));
    let main = repository(&procly, &files);

    let output = init(&procly, &main);

    assert_eq!(
        group_warning(&output),
        format!("procly: {GROUP_WARNING}, and web has port 3001 written in apps/web/.env.example.")
    );
}

#[test]
fn a_port_in_the_dev_script_of_a_covered_app_is_named_with_its_package_json() {
    let procly = Procly::new();
    let mut files = MONOREPO.to_vec();
    files[2] = (
        "apps/web/package.json",
        r#"{"name": "@acme/web", "scripts": {"dev": "next dev --port 3001"}}"#,
    );
    files.push(("apps/api/.env.example", "export PORT='3000'\n"));
    let main = repository(&procly, &files);

    let output = init(&procly, &main);

    assert_eq!(
        group_warning(&output),
        format!(
            "procly: {GROUP_WARNING}, and api has port 3000 written in apps/api/.env.example, web has port 3001 \
             written in the dev script of apps/web/package.json."
        )
    );
}

#[test]
fn a_dot_env_file_is_never_read() {
    let procly = Procly::new();
    let mut files = MONOREPO.to_vec();
    files.push(("apps/web/.env", "PORT=3001\n"));
    files.push(("apps/api/.env.local", "PORT=3000\n"));
    let main = repository(&procly, &files);

    let output = init(&procly, &main);

    assert_eq!(group_warning(&output), format!("procly: {GROUP_WARNING}."));
}

/// A Repository with no root `package.json`, such as a Rust workspace, and a JavaScript app with its own lockfile in a
/// subdirectory, every file of it tracked by git.
const NESTED_APP: [(&str, &str); 3] = [
    ("Cargo.toml", "[workspace]\n"),
    (
        "apps/desktop/package.json",
        r#"{"name": "procly-desktop", "scripts": {"dev": "vite", "build": "vite build"}}"#,
    ),
    ("apps/desktop/bun.lock", ""),
];

/// A Repository whose Main worktree holds the given files, all of them added to git.
fn tracked_repository(procly: &Procly, files: &[(&str, &str)]) -> PathBuf {
    let main = repository(procly, files);
    common::git(&main, &["add", "--all"]);
    main
}

/// The names of the Services of a proposal.
fn service_names(proposal: &str) -> Vec<String> {
    let table: toml::Table = proposal.parse().expect("the proposal is TOML");
    table["services"]
        .as_table()
        .expect("the proposal has a services table")
        .keys()
        .cloned()
        .collect()
}

#[test]
fn without_a_root_package_json_init_from_the_root_proposes_the_tracked_package_with_a_dev_script() {
    let procly = Procly::new();
    let main = tracked_repository(&procly, &NESTED_APP);

    let output = init(&procly, &main);

    let proposal = stdout(&output);
    assert_eq!(service_names(&proposal), ["procly-desktop"], "{proposal}");
    assert_eq!(
        run_and_port(&proposal, "procly-desktop"),
        ("cd apps/desktop && bun run dev".to_owned(), None)
    );
    assert_loads(&procly, &main, "procly-desktop");
}

#[test]
fn without_a_root_package_json_init_from_a_package_directory_proposes_that_package_only() {
    let procly = Procly::new();
    let mut files = NESTED_APP.to_vec();
    files.push((
        "site/package.json",
        r#"{"name": "site", "scripts": {"dev": "astro dev"}}"#,
    ));
    let main = tracked_repository(&procly, &files);

    let output = init(&procly, &main.join("apps/desktop"));

    let proposal = stdout(&output);
    assert_eq!(service_names(&proposal), ["procly-desktop"], "{proposal}");
    assert_eq!(
        run_and_port(&proposal, "procly-desktop"),
        ("cd apps/desktop && bun run dev".to_owned(), None)
    );
    assert_eq!(fs::read_to_string(main.join("procly.toml")).unwrap(), proposal);
}

#[test]
fn without_a_root_package_json_init_from_the_root_proposes_every_tracked_package_in_the_order_of_their_directories() {
    let procly = Procly::new();
    let mut files = NESTED_APP.to_vec();
    files.push((
        "site/package.json",
        r#"{"name": "site", "scripts": {"dev": "astro dev"}}"#,
    ));
    files.push(("apps/web/package.json", r#"{"scripts": {"dev": "next dev"}}"#));
    files.push((
        "apps/lib/package.json",
        r#"{"name": "lib", "scripts": {"build": "tsc"}}"#,
    ));
    files.push(("pnpm-lock.yaml", ""));
    let main = tracked_repository(&procly, &files);
    fs::create_dir(main.join("docs")).unwrap();

    let output = init(&procly, &main.join("docs"));

    let proposal = stdout(&output);
    let table: toml::Table = proposal.parse().unwrap();
    let runs: Vec<&str> = ["procly-desktop", "web", "site"]
        .iter()
        .map(|service| table["services"][*service]["run"].as_str().unwrap())
        .collect();
    assert_eq!(
        runs,
        [
            "cd apps/desktop && bun run dev",
            "cd apps/web && pnpm run dev",
            "cd site && pnpm run dev"
        ],
        "a lockfile of the package wins over the one at the root: {proposal}"
    );
    assert_eq!(service_names(&proposal).len(), 3, "{proposal}");
}

#[test]
fn an_untracked_or_ignored_package_json_is_never_proposed() {
    let procly = Procly::new();
    let mut files = NESTED_APP.to_vec();
    files.push((".gitignore", "vendor/\n"));
    let main = tracked_repository(&procly, &files);
    let dev = r#"{"name": "stray", "scripts": {"dev": "vite"}}"#;
    write(&main.join("apps/new/package.json"), dev);
    write(&main.join("vendor/tool/package.json"), dev);
    write(&main.join("apps/desktop/node_modules/vite/package.json"), dev);

    for from in [main.clone(), main.join("apps/new"), main.join("vendor/tool")] {
        let output = init(&procly, &from);
        assert_eq!(
            service_names(&stdout(&output)),
            ["procly-desktop"],
            "from {}",
            from.display()
        );
        fs::remove_file(main.join("procly.toml")).unwrap();
    }
}

#[test]
fn a_tracked_package_in_a_directory_with_its_own_procly_toml_is_a_separate_project_skipped_with_a_note() {
    let procly = Procly::new();
    let mut files = NESTED_APP.to_vec();
    files.push((
        "sample/project/package.json",
        r#"{"name": "acme-shop", "scripts": {"dev": "python3 -m http.server 24080"}}"#,
    ));
    files.push(("sample/project/procly.toml", "[services.web]\nrun = 'sleep 1000'\n"));
    let main = tracked_repository(&procly, &files);

    let output = init(&procly, &main);

    assert_eq!(
        service_names(&stdout(&output)),
        ["procly-desktop"],
        "{}",
        stdout(&output)
    );
    assert!(
        stderr(&output).contains(
            "procly: sample/project/package.json has a dev script, but sample/project holds its own procly.toml, so it \
             is a separate project and is not proposed."
        ),
        "{}",
        stderr(&output)
    );
}

#[test]
fn without_a_root_package_json_nor_a_tracked_one_with_a_dev_script_init_says_where_it_looked() {
    let procly = Procly::new();
    let main = tracked_repository(
        &procly,
        &[
            ("Cargo.toml", "[workspace]\n"),
            (
                "tools/package.json",
                r#"{"name": "tools", "scripts": {"build": "tsc"}}"#,
            ),
        ],
    );
    write(&main.join("untracked/package.json"), r#"{"scripts": {"dev": "vite"}}"#);

    let output = procly.procly_in(&main, &["init"]);

    assert_eq!(output.status.code(), Some(125));
    assert!(
        stderr(&output).contains(
            "has no package.json with a dev script at its root nor among the files git tracks, and no compose file, \
             so there is nothing to propose"
        ),
        "{}",
        stderr(&output)
    );
    assert!(!main.join("procly.toml").exists());
}

#[test]
fn packages_found_below_the_root_are_named_in_the_order_of_their_directories() {
    let procly = Procly::new();
    // git lists `x-y/package.json` before `x/y/package.json`, a directory comes before the ones next to it.
    let main = tracked_repository(
        &procly,
        &[
            ("x-y/package.json", r#"{"name": "app", "scripts": {"dev": "vite"}}"#),
            ("x/y/package.json", r#"{"name": "app", "scripts": {"dev": "vite"}}"#),
        ],
    );

    let proposal = stdout(&init(&procly, &main));

    assert_eq!(
        [run_and_port(&proposal, "app").0, run_and_port(&proposal, "app-2").0],
        ["cd x/y && npm run dev", "cd x-y && npm run dev"],
        "{proposal}"
    );
}
