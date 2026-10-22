//! Logical resources of Compose services, created by Drivers inside one shared container (ADR 0005). The tests
//! without the `docker_` prefix need no container: names and URLs are worked out from the configuration alone.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

use common::{
    Procly, assert_success, free_port, git, git_repository, git_worktree_add, require_docker, stderr, stdout,
    test_server,
};

/// A Repository with a Postgres Compose service holding 2 resources, and an `api` reading both URLs.
fn config(port: u16, driver: &str) -> String {
    format!(
        "[services.db]\ncompose = 'postgres'\ndriver = '{driver}'\nports = {{ db = {port} }}\n\
         [services.db.resources.dev]\nname = 'app'\n\
         [services.db.resources.test]\nname = 'app_test'\n\
         [services.api]\nrun = '{server} --port {{ports.http}}'\nports = {{ http = {api} }}\n\
         [services.api.env]\nDATABASE_URL = '{{services.db.resources.dev.url}}'\n\
         TEST_DATABASE_URL = '{{services.db.resources.test.url}}'\n\
         DATABASE_NAME = '{{services.db.resources.dev.name}}'\n",
        server = test_server().display(),
        api = free_port(),
    )
}

/// The value of `name` in the `export NAME='value'` lines of `procly env`.
fn exported(output: &std::process::Output, name: &str) -> String {
    assert_success(output);
    let text = stdout(output);
    let prefix = format!("export {name}='");
    text.lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .and_then(|value| value.strip_suffix('\''))
        .unwrap_or_else(|| panic!("no {name} in {text}"))
        .to_owned()
}

/// A Repository `app` with a second Worktree `app-other` (slug `app_other`, so its resources end in `__app_other`), and the port of its database.
fn repository(procly: &Procly, driver: &str) -> (PathBuf, PathBuf, u16) {
    let main = git_repository(procly.scratch(), "app");
    let other = git_worktree_add(&main, "app-other");
    let port = free_port();
    fs::write(main.join("procly.toml"), config(port, driver)).expect("write the config");
    fs::write(other.join("procly.toml"), config(port, driver)).expect("write the config");
    (main, other, port)
}

/// The port of the health check `procly up` printed, at the end of the first Lease of its line.
fn port_of(output: &std::process::Output) -> u16 {
    assert_success(output);
    let line = stdout(output);
    line.trim_end()
        .split(", ")
        .next()
        .unwrap_or_default()
        .rsplit(' ')
        .next()
        .and_then(|port| port.parse().ok())
        .unwrap_or_else(|| panic!("no port at the end of {line:?}"))
}

/// A Driver that only has a URL, for the tests that read it.
fn write_driver(dir: &Path, name: &str, url: &str) {
    write_driver_file(
        dir,
        name,
        &format!("url = '{url}'\ncheck = 'true'\ncreate = 'true'\ndrop = 'true'\n"),
    );
}

#[test]
fn each_worktree_reads_its_own_database_url_through_a_derived_variable() {
    let procly = Procly::new();
    let (main, other, port) = repository(&procly, "postgres");

    let in_main = procly.procly_in(&main, &["env", "api"]);
    let in_other = procly.procly_in(&other, &["env", "api"]);

    // The Main worktree keeps the names of the configuration, the others append their slug. Both reach the one container,
    // which holds the Leases of the Main worktree.
    let url = |name: &str| format!("postgresql://postgres:postgres@localhost:{port}/{name}");
    assert_eq!(exported(&in_main, "DATABASE_URL"), url("app"));
    assert_eq!(exported(&in_main, "TEST_DATABASE_URL"), url("app_test"));
    assert_eq!(exported(&in_main, "DATABASE_NAME"), "app");
    assert_eq!(exported(&in_other, "DATABASE_URL"), url("app__app_other"));
    assert_eq!(exported(&in_other, "TEST_DATABASE_URL"), url("app_test__app_other"));
    assert_eq!(exported(&in_other, "DATABASE_NAME"), "app__app_other");
}

#[test]
fn the_connection_urls_are_mirrored_in_the_env_file_of_the_worktree() {
    let procly = Procly::new();
    let (_, other, port) = repository(&procly, "postgres");

    assert_success(&procly.procly_in(&other, &["up", "api"]));

    let env_file = fs::read_to_string(other.join(".env.procly")).expect("read the Env file");
    assert!(
        env_file.contains(&format!(
            "DATABASE_URL=postgresql://postgres:postgres@localhost:{port}/app__app_other\n"
        )),
        "{env_file}"
    );
    assert!(env_file.contains("/app_test__app_other\n"), "{env_file}");
}

#[test]
fn a_driver_of_the_repository_takes_precedence_over_the_user_and_the_bundled_ones() {
    let procly = Procly::new();
    let (main, _, port) = repository(&procly, "postgres");
    let url = |output: &std::process::Output| exported(output, "DATABASE_URL");

    // Bundled when nobody wrote one.
    assert!(url(&procly.procly_in(&main, &["env", "api"])).starts_with("postgresql://postgres:postgres@"));

    // The user's directory is next to the user settings.
    write_driver(
        &procly.config_dir().join("drivers"),
        "postgres",
        "pg-user://localhost:{port}/{resource}",
    );
    assert_eq!(
        url(&procly.procly_in(&main, &["env", "api"])),
        format!("pg-user://localhost:{port}/app")
    );

    // The Repository wins over both, and is read from the Worktree that asks.
    write_driver(
        &main.join(".procly/drivers"),
        "postgres",
        "pg-repository://app:secret@localhost:{port}/{resource}",
    );
    assert_eq!(
        url(&procly.procly_in(&main, &["env", "api"])),
        format!("pg-repository://app:secret@localhost:{port}/app")
    );
}

#[test]
fn a_driver_that_does_not_exist_or_a_name_too_long_for_it_fails_before_anything_starts() {
    let procly = Procly::new();
    let (main, other, _) = repository(&procly, "mongo");

    let missing = procly.procly_in(&main, &["env", "api"]);
    assert_eq!(missing.status.code(), Some(125));
    let message = stderr(&missing);
    assert!(message.contains("no Driver named mongo"), "{message}");
    assert!(message.contains(".procly/drivers"), "{message}");

    // Postgres cuts a name past 63 bytes, which would make 2 Worktrees share a database.
    let long = "a".repeat(60);
    let port = free_port();
    let text = config(port, "postgres").replace("name = 'app'", &format!("name = '{long}'"));
    fs::write(other.join("procly.toml"), text).expect("write the config");
    let too_long = procly.procly_in(&other, &["env", "api"]);
    assert_eq!(too_long.status.code(), Some(125));
    assert!(stderr(&too_long).contains("its limit is 63"), "{}", stderr(&too_long));
}

/// Removes the containers, network and volumes of a test project when the test ends, passed or failed.
struct Project {
    name: String,
    dir: PathBuf,
}

impl Project {
    fn try_docker(&self, args: &[&str]) -> std::io::Result<std::process::Output> {
        Command::new("docker")
            .args(["compose", "-p", &self.name])
            .args(args)
            .current_dir(&self.dir)
            .env("DB_PORT", "1")
            .output()
    }

    /// Runs a script inside the container of service `db` and returns what it printed.
    fn exec(&self, script: &str) -> String {
        let output = self
            .try_docker(&["exec", "-T", "db", "sh", "-c", script])
            .expect("docker runs");
        stdout(&output)
    }

    fn is_running(&self) -> bool {
        let output = self
            .try_docker(&["ps", "--status", "running", "--services"])
            .expect("docker runs");
        stdout(&output).lines().any(|service| service == "db")
    }

    fn container(&self) -> String {
        let output = self.try_docker(&["ps", "-a", "-q", "db"]).expect("docker runs");
        stdout(&output).trim().to_owned()
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        // A test that needs no Docker never started anything, and may run where Docker is absent.
        let _ = self.try_docker(&["down", "--volumes", "--remove-orphans"]);
    }
}

/// A Repository with a compose file, a `procly.toml` and a second Worktree, in a project of its own.
struct Fixture {
    // First, so that the containers are removed while the working directories still exist.
    project: Project,
    procly: Procly,
    main: PathBuf,
    other: PathBuf,
    port: u16,
}

fn project_name() -> String {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    format!("pcd-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed))
}

/// The Driver of an imaginary technology whose resources are directories of the data volume: a new technology is
/// only a file.
const DIRECTORY_DRIVER: &str = "url = 'dir://localhost:{port}/{resource}'\n\
    check = 'test -d /data/res/{resource}'\n\
    create = 'mkdir -p /data/res/{resource}'\n\
    copy = 'cp -r /data/res/{template} /data/res/{resource}'\n\
    drop = 'rm -r -f /data/res/{resource}'\n";

/// A compose file whose service `db` runs a TCP listener in `alpine`, with a data volume.
fn alpine_compose(project: &str) -> String {
    format!(
        "name: {project}\nservices:\n  db:\n    image: alpine\n    init: true\n    command: [\"nc\", \"-lk\", \"-p\", \"80\", \"-e\", \"/bin/cat\"]\n    \
         ports:\n      - \"${{DB_PORT:-1}}:80\"\n    volumes:\n      - data:/data\nvolumes:\n  data:\n"
    )
}

/// A Repository on the directory Driver, with `resources` as TOML under `[services.db.resources]`, and a second
/// Worktree. The Driver and the configuration are committed, so both Worktrees hold them.
fn directory_fixture(resources: &str) -> Fixture {
    let fixture = logical_fixture(alpine_compose, "dir", resources);
    write_driver_file(&fixture.main.join(".procly/drivers"), "dir", DIRECTORY_DRIVER);
    commit_and_add_worktree(fixture)
}

/// A Repository whose service `db` is the compose service `db` of `compose(project)`, on the Driver `driver`.
/// Nothing is committed yet and the second Worktree is not added, see [`commit_and_add_worktree`].
fn logical_fixture(compose: fn(&str) -> String, driver: &str, resources: &str) -> Fixture {
    let procly = Procly::new();
    let (name, port) = (project_name(), free_port());
    let main = git_repository(procly.scratch(), "app");
    let config = format!(
        "[services.db]\ncompose = 'db'\ndriver = '{driver}'\nports = {{ db = {port} }}\n\
         env = {{ DB_PORT = '{{services.db.ports.db}}' }}\n{resources}"
    );
    fs::write(main.join("compose.yaml"), compose(&name)).expect("write the compose file");
    fs::write(main.join("procly.toml"), config).expect("write the config");
    Fixture {
        procly,
        project: Project {
            name,
            dir: main.clone(),
        },
        other: main.clone(),
        main,
        port,
    }
}

/// Commits what the Repository holds and adds the second Worktree, so that both hold the same files.
fn commit_and_add_worktree(mut fixture: Fixture) -> Fixture {
    let main = fixture.main.clone();
    git(&main, &["add", "."]);
    git(&main, &["commit", "--quiet", "--message", "compose"]);
    fixture.other = git_worktree_add(&main, "app-other");
    fixture
}

fn write_driver_file(dir: &Path, name: &str, text: &str) {
    fs::create_dir_all(dir).expect("create the Drivers directory");
    fs::write(dir.join(format!("{name}.toml")), text).expect("write the Driver");
}

/// The resources of the data volume, sorted.
fn resources(project: &Project) -> Vec<String> {
    let mut found: Vec<String> = project
        .exec("ls /data/res 2>/dev/null")
        .lines()
        .map(str::to_owned)
        .collect();
    found.sort();
    found
}

const DEV_AND_TEST: &str =
    "[services.db.resources.dev]\nname = 'app'\n[services.db.resources.test]\nname = 'app_test'\n";

#[test]
fn docker_2_worktrees_get_distinct_resources_in_the_same_container_from_a_driver_file_of_the_repository() {
    if !require_docker() {
        return;
    }
    let fixture = directory_fixture(DEV_AND_TEST);

    let first = fixture.procly.procly_in(&fixture.main, &["up", "db"]);

    assert_success(&first);
    // The declared port, unless another process took it between the choice of the test and the start.
    let port = port_of(&first);
    assert_eq!(
        stdout(&first),
        format!("db started dir://localhost:{port} port {port}\n")
    );
    // Several Logical resources per Service, under the names of the configuration in the Main worktree.
    assert_eq!(resources(&fixture.project), ["app", "app_test"]);
    let container = fixture.project.container();

    let second = fixture.procly.procly_in(&fixture.other, &["up", "db"]);

    assert_success(&second);
    assert_eq!(
        stdout(&second),
        format!("db reused dir://localhost:{port} port {port}\n")
    );
    assert_eq!(
        resources(&fixture.project),
        ["app", "app__app_other", "app_test", "app_test__app_other"]
    );
    // The one container, not one recreated for the second Worktree.
    assert_eq!(fixture.project.container(), container);
}

/// A command Seed that writes `marker` in the new resource through the container, and notes on the host that it ran.
fn marking_seed(project: &Project, ran: &Path) -> String {
    format!(
        "docker compose -p {} exec -T db sh -c \"echo from-seed > /data/res/$PROCLY_RESOURCE_NAME/marker\" \
         && echo \"$PROCLY_RESOURCE $PROCLY_RESOURCE_NAME $PROCLY_RESOURCE_URL\" >> {}",
        project.name,
        ran.display()
    )
}

#[test]
fn docker_each_seed_variant_gives_a_new_resource_its_content_and_an_existing_one_is_left_alone() {
    if !require_docker() {
        return;
    }
    let fixture = directory_fixture("[services.db.resources.plain]\nname = 'app_plain'\n");
    let ran = fixture.procly.scratch().join("seed-ran");
    assert_success(&fixture.procly.procly_in(&fixture.main, &["up", "db"]));
    // The template is a resource the container already holds, as a database a team restored once would be.
    fixture
        .project
        .exec("mkdir -p /data/res/base && echo from-template > /data/res/base/marker");
    fs::write(
        fixture.main.join("procly.local.toml"),
        format!(
            "[services.db.resources.copied]\nname = 'app_copied'\nseed = {{ template = 'base' }}\n\
             [services.db.resources.filled]\nname = 'app_filled'\nseed = {{ run = '{}' }}\n",
            marking_seed(&fixture.project, &ran)
        ),
    )
    .expect("write the local config");

    let output = fixture.procly.procly_in(&fixture.main, &["up", "db"]);

    assert_success(&output);
    let content = |resource: &str| fixture.project.exec(&format!("cat /data/res/{resource}/marker 2>&1"));
    // Empty: nothing in it. Template: a copy of it. Command: what the command wrote, told which resource it fills.
    assert_eq!(fixture.project.exec("ls /data/res/app_plain"), "");
    assert_eq!(content("app_copied"), "from-template\n");
    assert_eq!(content("app_filled"), "from-seed\n");
    let port = port_of(&output);
    assert_eq!(
        fs::read_to_string(&ran).expect("the seed ran"),
        format!("filled app_filled dir://localhost:{port}/app_filled\n")
    );

    // A resource that exists is neither seeded again nor touched, whatever it holds.
    fixture.project.exec("echo edited > /data/res/app_copied/marker");
    assert_success(&fixture.procly.procly_in(&fixture.main, &["up", "db"]));
    assert_eq!(content("app_copied"), "edited\n");
    assert_eq!(fs::read_to_string(&ran).unwrap().lines().count(), 1);
}

#[test]
fn docker_a_seed_that_fails_drops_the_resource_it_just_created_so_that_the_next_up_seeds_it_again() {
    if !require_docker() {
        return;
    }
    let fixture = directory_fixture("[services.db.resources.plain]\nname = 'app_plain'\n");
    assert_success(&fixture.procly.procly_in(&fixture.main, &["up", "db"]));
    let failing = |run: &str| {
        format!("[services.db.resources.filled]\nname = 'app_filled'\nseed = {{ run = \"{run}\", timeout = '1m' }}\n")
    };
    fs::write(
        fixture.main.join("procly.local.toml"),
        failing("echo no luck >&2; exit 7"),
    )
    .expect("write the config");

    let failed = fixture.procly.procly_in(&fixture.main, &["up", "db"]);

    assert_eq!(failed.status.code(), Some(125));
    let message = stderr(&failed);
    assert!(
        message.contains("the seed of resource app_filled of service db failed"),
        "{message}"
    );
    assert!(message.contains("exited with 7"), "{message}");
    assert!(message.contains("no luck"), "{message}");
    assert!(message.contains("has been dropped"), "{message}");
    assert!(
        fixture.procly.daemon_log().contains("has been dropped"),
        "the Daemon log does not say it"
    );
    // The resource that existed before is still there, the failed one is not.
    assert_eq!(resources(&fixture.project), ["app_plain"]);

    fs::write(fixture.main.join("procly.local.toml"), failing("true")).expect("write the config");
    assert_success(&fixture.procly.procly_in(&fixture.main, &["up", "db"]));
    assert_eq!(resources(&fixture.project), ["app_filled", "app_plain"]);
}

#[test]
fn docker_a_seed_that_never_ends_is_killed_at_its_timeout_and_drops_its_resource() {
    if !require_docker() {
        return;
    }
    let fixture = directory_fixture("[services.db.resources.plain]\nname = 'app_plain'\n");
    assert_success(&fixture.procly.procly_in(&fixture.main, &["up", "db"]));
    fs::write(
        fixture.main.join("procly.local.toml"),
        "[services.db.resources.slow]\nname = 'app_slow'\nseed = { run = 'sleep 30', timeout = '1s' }\n",
    )
    .expect("write the config");

    let failed = fixture.procly.procly_in(&fixture.main, &["up", "db"]);

    assert_eq!(failed.status.code(), Some(125));
    let message = stderr(&failed);
    assert!(message.contains("did not end within 1s"), "{message}");
    assert_eq!(resources(&fixture.project), ["app_plain"]);
}

#[test]
fn docker_a_driver_command_running_past_the_driver_timeout_fails_up() {
    if !require_docker() {
        return;
    }
    let fixture = logical_fixture(
        alpine_compose,
        "slow",
        "[services.db.resources.dev]\nname = 'app'\n[settings]\ndriver_timeout = '1s'\n",
    );
    write_driver_file(
        &fixture.main.join(".procly/drivers"),
        "slow",
        &DIRECTORY_DRIVER.replace("create = 'mkdir -p /data/res/{resource}'", "create = 'sleep 30'"),
    );
    let fixture = commit_and_add_worktree(fixture);

    let failed = fixture.procly.procly_in(&fixture.main, &["up", "db"]);

    assert_eq!(failed.status.code(), Some(125));
    let message = stderr(&failed);
    assert!(message.contains("did not end within 1s"), "{message}");
}

#[test]
fn docker_a_template_seed_needs_a_driver_with_a_copy_command_and_an_existing_template() {
    if !require_docker() {
        return;
    }
    let fixture = directory_fixture("[services.db.resources.plain]\nname = 'app_plain'\n");
    assert_success(&fixture.procly.procly_in(&fixture.main, &["up", "db"]));
    fs::write(
        fixture.main.join("procly.local.toml"),
        "[services.db.resources.copied]\nname = 'app_copied'\nseed = { template = 'missing' }\n",
    )
    .expect("write the config");

    let missing = fixture.procly.procly_in(&fixture.main, &["up", "db"]);

    assert_eq!(missing.status.code(), Some(125));
    let message = stderr(&missing);
    assert!(message.contains("could not create resource app_copied"), "{message}");
    assert_eq!(resources(&fixture.project), ["app_plain"]);

    let no_copy = DIRECTORY_DRIVER.replace("copy = 'cp -r /data/res/{template} /data/res/{resource}'\n", "");
    write_driver_file(&fixture.main.join(".procly/drivers"), "dir", &no_copy);
    let refused = fixture.procly.procly_in(&fixture.main, &["up", "db"]);
    assert_eq!(refused.status.code(), Some(125));
    assert!(
        stderr(&refused).contains("has no `copy` command"),
        "{}",
        stderr(&refused)
    );
}

/// A compose file whose service `db` is the Postgres image, healthy once the final server accepts TCP connections.
fn postgres_compose(project: &str) -> String {
    format!(
        "name: {project}\nservices:\n  db:\n    image: postgres:17-alpine\n    environment:\n      POSTGRES_PASSWORD: postgres\n    \
         ports:\n      - \"${{DB_PORT:-1}}:5432\"\n    healthcheck:\n      test: [\"CMD-SHELL\", \"pg_isready -h 127.0.0.1 -U postgres\"]\n      \
         interval: 1s\n      timeout: 3s\n      retries: 60\n"
    )
}

/// A Repository on the bundled Postgres Driver, and a second Worktree.
fn postgres_fixture(resources: &str) -> Fixture {
    commit_and_add_worktree(logical_fixture(postgres_compose, "postgres", resources))
}

/// What a query prints on database `database` of the container, unaligned and without headers.
fn query(project: &Project, database: &str, sql: &str) -> String {
    project
        .exec(&format!(
            "psql --username postgres --dbname {database} --tuples-only --no-align --command \"{sql}\" 2>&1"
        ))
        .trim()
        .to_owned()
}

/// The databases of the container Procly created for the Repository, sorted.
fn databases(project: &Project) -> Vec<String> {
    query(
        project,
        "postgres",
        "SELECT datname FROM pg_database WHERE datname LIKE 'app%' ORDER BY 1",
    )
    .lines()
    .map(str::to_owned)
    .collect()
}

#[test]
fn docker_postgres_2_worktrees_get_distinct_databases_for_dev_and_test_in_one_container() {
    if !require_docker() {
        return;
    }
    let fixture = postgres_fixture(DEV_AND_TEST);

    let first = fixture.procly.procly_in(&fixture.main, &["up", "db"]);
    assert_success(&first);
    let container = fixture.project.container();
    let second = fixture.procly.procly_in(&fixture.other, &["up", "db"]);
    assert_success(&second);

    assert_eq!(
        databases(&fixture.project),
        ["app", "app__app_other", "app_test", "app_test__app_other"]
    );
    assert_eq!(fixture.project.container(), container);
    // The container publishes the Lease of the Main worktree, which every Worktree reads.
    let port = exported(&fixture.procly.procly_in(&fixture.other, &["env", "db"]), "PORT");
    assert_eq!(port, fixture.port.to_string());
}

#[test]
fn docker_postgres_each_seed_variant_produces_the_expected_content() {
    if !require_docker() {
        return;
    }
    let fixture = postgres_fixture("[services.db.resources.plain]\nname = 'app_plain'\n");
    assert_success(&fixture.procly.procly_in(&fixture.main, &["up", "db"]));
    // The template is a database that exists already, with a table and a row.
    let project = &fixture.project;
    project.exec("createdb --username postgres app_base");
    query(
        project,
        "app_base",
        "CREATE TABLE people (name text); INSERT INTO people VALUES ('ada')",
    );
    let seed = format!(
        "docker compose -p {} exec -T db psql --username postgres --dbname $PROCLY_RESOURCE_NAME --command \
         'CREATE TABLE seeded (n int); INSERT INTO seeded VALUES (42)'",
        project.name
    );
    fs::write(
        fixture.main.join("procly.local.toml"),
        format!(
            "[services.db.resources.copied]\nname = 'app_copied'\nseed = {{ template = 'app_base' }}\n\
             [services.db.resources.filled]\nname = 'app_filled'\nseed = {{ run = \"{seed}\" }}\n"
        ),
    )
    .expect("write the local config");

    assert_success(&fixture.procly.procly_in(&fixture.main, &["up", "db"]));

    // Empty: no table. Template: the table and the row of the template. Command: what the command created.
    let tables = "SELECT count(*) FROM information_schema.tables WHERE table_schema = 'public'";
    assert_eq!(query(project, "app_plain", tables), "0");
    assert_eq!(query(project, "app_copied", "SELECT name FROM people"), "ada");
    assert_eq!(query(project, "app_filled", "SELECT n FROM seeded"), "42");
    assert_eq!(query(project, "app_filled", tables), "1");
}

#[test]
fn docker_resources_a_worktree_cannot_create_never_stop_a_container_other_worktrees_still_need() {
    if !require_docker() {
        return;
    }
    let fixture = directory_fixture("[services.db.resources.plain]\nname = 'app_plain'\n");
    assert_success(&fixture.procly.procly_in(&fixture.main, &["up", "db"]));
    // The container was stopped from outside Procly, so the next `up` starts it again for the Worktree that needs it.
    assert!(fixture.project.try_docker(&["stop", "db"]).unwrap().status.success());
    assert!(!fixture.project.is_running());
    fs::write(
        fixture.other.join("procly.local.toml"),
        "[services.db.resources.broken]\nname = 'app_broken'\nseed = { run = 'exit 1' }\n",
    )
    .expect("write the config");

    let failed = fixture.procly.procly_in(&fixture.other, &["up", "db"]);

    assert_eq!(failed.status.code(), Some(125));
    assert!(
        fixture.project.is_running(),
        "a seed of one Worktree stopped the container the Main worktree needs"
    );
    // The Main worktree is still counted, it keeps the container until it lets go.
    assert_success(&fixture.procly.procly_in(&fixture.main, &["up", "db"]));
}

/// The bundled Redis Driver with a capacity of `capacity` indexes, as a Repository that raises or lowers it writes it.
fn redis_driver(capacity: u32) -> String {
    include_str!("../drivers/redis.toml").replace("capacity = 16", &format!("capacity = {capacity}"))
}

/// A `procly.toml` with a Redis Compose service `cache` holding one resource, and an `api` reading its URL. `settings`
/// is the text of a `[settings]` table or nothing, and `isolation` the line of the Service that sets its mode or nothing.
fn redis_config(settings: &str, isolation: &str, port: u16) -> String {
    format!(
        "{settings}[services.cache]\ncompose = 'redis'\ndriver = 'redis'\n{isolation}ports = {{ redis = {port} }}\n\
         [services.cache.resources.main]\n\
         [services.api]\nrun = '{server} --port {{ports.http}}'\nports = {{ http = {api} }}\n\
         [services.api.env]\nREDIS_URL = '{{services.cache.resources.main.url}}'\n\
         REDIS_DB = '{{services.cache.resources.main.name}}'\n",
        server = test_server().display(),
        api = free_port(),
    )
}

/// A Repository `app` with `others` more Worktrees named `app-a`, `app-b`, and so on, all holding `config` and the
/// Redis Driver of `capacity` indexes, as a Driver of the Repository.
fn redis_repository(procly: &Procly, config: &str, capacity: u32, others: usize) -> (PathBuf, Vec<PathBuf>) {
    let main = git_repository(procly.scratch(), "app");
    let others: Vec<_> = (0..others)
        .map(|n| {
            git_worktree_add(
                &main,
                &format!("app-{}", char::from(b'a' + u8::try_from(n).expect("a few Worktrees"))),
            )
        })
        .collect();
    for worktree in std::iter::once(&main).chain(&others) {
        fs::write(worktree.join("procly.toml"), config).expect("write the config");
        write_driver_file(&worktree.join(".procly/drivers"), "redis", &redis_driver(capacity));
    }
    (main, others)
}

/// The Redis database index `procly env api` puts in the URL.
fn redis_index(procly: &Procly, worktree: &Path) -> u32 {
    let url = exported(&procly.procly_in(worktree, &["env", "api"]), "REDIS_URL");
    url.rsplit('/')
        .next()
        .and_then(|index| index.parse().ok())
        .unwrap_or_else(|| panic!("no database index at the end of {url}"))
}

#[test]
fn redis_2_worktrees_get_distinct_database_indexes_and_the_main_worktree_keeps_the_lowest() {
    let procly = Procly::new();
    let port = free_port();
    let (main, others) = redis_repository(&procly, &redis_config("", "", port), 16, 2);

    // The Main worktree asks last, and still has the index every app that never picks one reads.
    let in_a = redis_index(&procly, &others[0]);
    let in_b = redis_index(&procly, &others[1]);
    let in_main = redis_index(&procly, &main);

    assert_eq!([in_main, in_a, in_b], [0, 1, 2]);
    let url = exported(&procly.procly_in(&others[0], &["env", "api"]), "REDIS_URL");
    assert_eq!(url, format!("redis://localhost:{port}/1"));
    // The name of a resource a Driver numbers is its index.
    assert_eq!(
        exported(&procly.procly_in(&others[0], &["env", "api"]), "REDIS_DB"),
        "1"
    );

    // The indexes are the same after the Daemon restarted, whichever Worktree asks first.
    procly.stop_daemon();
    assert_eq!(redis_index(&procly, &others[1]), 2);
    assert_eq!(redis_index(&procly, &others[0]), 1);
    assert_eq!(redis_index(&procly, &main), 0);
}

#[test]
fn a_container_with_no_redis_index_left_is_a_clear_error_naming_both_ways_out() {
    let procly = Procly::new();
    let (main, others) = redis_repository(&procly, &redis_config("", "", free_port()), 2, 2);
    assert_eq!(redis_index(&procly, &main), 0);
    assert_eq!(redis_index(&procly, &others[0]), 1);

    let full = procly.procly_in(&others[1], &["env", "api"]);

    assert_eq!(full.status.code(), Some(125));
    let message = stderr(&full);
    assert!(
        message.contains("all 2 indexes of service cache are taken"),
        "{message}"
    );
    assert!(message.contains(".procly/drivers/redis.toml"), "{message}");
    assert!(message.contains("isolation = \"shared\""), "{message}");
    // The Worktrees that have an index keep it.
    assert_eq!(redis_index(&procly, &others[0]), 1);
}

#[test]
fn the_isolation_of_the_repository_config_overrides_the_logical_default_of_the_service() {
    let indexes = |settings: &str, isolation: &str| {
        let procly = Procly::new();
        let (main, others) = redis_repository(&procly, &redis_config(settings, isolation, free_port()), 16, 2);
        [&main, &others[0], &others[1]].map(|worktree| redis_index(&procly, worktree))
    };

    // Logical by default, with a Driver.
    assert_eq!(indexes("", ""), [0, 1, 2]);
    // Shared by the Service, or by the Repository for every Service with a Driver: the same index everywhere.
    assert_eq!(indexes("", "isolation = 'shared'\n"), [0, 0, 0]);
    assert_eq!(indexes("[settings]\nisolation = 'shared'\n", ""), [0, 0, 0]);
    // The Service wins over the Repository.
    assert_eq!(
        indexes("[settings]\nisolation = 'shared'\n", "isolation = 'logical'\n"),
        [0, 1, 2]
    );
    assert_eq!(
        indexes("[settings]\nisolation = 'logical'\n", "isolation = 'shared'\n"),
        [0, 0, 0]
    );
}

#[test]
fn a_shared_redis_service_gives_every_worktree_the_same_index_and_uses_no_other_capacity() {
    let procly = Procly::new();
    // A container holding a single index, 3 Worktrees on it.
    let config = redis_config("", "isolation = 'shared'\n", free_port());
    let (main, others) = redis_repository(&procly, &config, 1, 2);

    // The Worktree that asks first is not the Main worktree, and its index is the one the Main worktree reads.
    assert_eq!(redis_index(&procly, &others[1]), 0);
    assert_eq!(redis_index(&procly, &others[0]), 0);
    assert_eq!(redis_index(&procly, &main), 0);
}

#[test]
fn a_shared_service_with_a_postgres_driver_gives_every_worktree_the_names_of_the_main_worktree() {
    let procly = Procly::new();
    let (main, other, port) = repository(&procly, "postgres");
    let shared =
        config(port, "postgres").replace("driver = 'postgres'\n", "driver = 'postgres'\nisolation = 'shared'\n");
    fs::write(other.join("procly.toml"), shared).expect("write the config");

    let in_other = procly.procly_in(&other, &["env", "api"]);
    let in_main = procly.procly_in(&main, &["env", "api"]);

    let url = |name: &str| format!("postgresql://postgres:postgres@localhost:{port}/{name}");
    // The Worktree that shares reads the Main worktree's database, and the Main worktree is not changed.
    assert_eq!(exported(&in_other, "DATABASE_URL"), url("app"));
    assert_eq!(exported(&in_other, "TEST_DATABASE_URL"), url("app_test"));
    assert_eq!(exported(&in_main, "DATABASE_URL"), url("app"));
}

#[test]
fn a_resource_name_is_refused_for_a_driver_that_numbers_and_required_for_one_that_names() {
    let procly = Procly::new();
    let (main, _) = redis_repository(&procly, &redis_config("", "", free_port()), 16, 0);
    let named = redis_config("", "", free_port()).replace("resources.main]\n", "resources.main]\nname = 'app'\n");
    fs::write(main.join("procly.toml"), named).expect("write the config");

    let refused = procly.procly_in(&main, &["env", "api"]);

    assert_eq!(refused.status.code(), Some(125));
    let message = stderr(&refused);
    assert!(message.contains("numbers its resources itself, remove it"), "{message}");

    let nameless = config(free_port(), "postgres").replace("name = 'app'\n", "");
    fs::write(main.join("procly.toml"), nameless).expect("write the config");
    let required = procly.procly_in(&main, &["env", "api"]);
    assert_eq!(required.status.code(), Some(125));
    assert!(
        stderr(&required).contains("sets no `name`, which the postgres Driver needs"),
        "{}",
        stderr(&required)
    );
}

/// A compose file whose service `db` is the Redis image, healthy once the server answers.
fn redis_compose(project: &str) -> String {
    format!(
        "name: {project}\nservices:\n  db:\n    image: redis:7-alpine\n    ports:\n      - \"${{DB_PORT:-1}}:6379\"\n    healthcheck:\n      \
         test: [\"CMD-SHELL\", \"redis-cli ping | grep -q PONG\"]\n      interval: 1s\n      timeout: 3s\n      retries: 60\n"
    )
}

#[test]
fn docker_redis_2_worktrees_keep_their_keys_in_distinct_databases_of_one_container() {
    if !require_docker() {
        return;
    }
    let api = "[services.db.resources.main]\n[services.api]\nrun = 'true'\n[services.api.env]\n\
               REDIS_URL = '{services.db.resources.main.url}'\n";
    let fixture = commit_and_add_worktree(logical_fixture(redis_compose, "redis", api));
    let index = |worktree: &Path| {
        let url = exported(&fixture.procly.procly_in(worktree, &["env", "api"]), "REDIS_URL");
        url.rsplit('/').next().expect("an index").to_owned()
    };

    assert_success(&fixture.procly.procly_in(&fixture.main, &["up", "db"]));
    let container = fixture.project.container();
    assert_success(&fixture.procly.procly_in(&fixture.other, &["up", "db"]));
    assert_eq!(fixture.project.container(), container);

    let (in_main, in_other) = (index(&fixture.main), index(&fixture.other));
    assert_eq!((in_main.as_str(), in_other.as_str()), ("0", "1"));
    let redis = |database: &str, command: &str| {
        fixture
            .project
            .exec(&format!("redis-cli -n {database} {command}"))
            .trim()
            .to_owned()
    };
    assert_eq!(redis(&in_main, "set session main-worktree"), "OK");
    // The other Worktree finds nothing under the key, and its own key never reaches the Main worktree.
    assert_eq!(redis(&in_other, "get session"), "");
    assert_eq!(redis(&in_other, "set session other-worktree"), "OK");
    assert_eq!(redis(&in_main, "get session"), "main-worktree");
    assert_eq!(redis(&in_other, "get session"), "other-worktree");
}

fn remove_worktree(main: &Path, worktree: &Path) {
    git(main, &["worktree", "remove", "--force", &worktree.to_string_lossy()]);
}

#[test]
fn gc_lists_the_logical_resources_of_removed_worktrees_but_never_those_of_a_pinned_branch() {
    let procly = Procly::new();
    let config = format!("{}[pinned.app-b]\n", redis_config("", "", free_port()));
    let (main, others) = redis_repository(&procly, &config, 16, 3);
    let indexes: Vec<u32> = others.iter().map(|worktree| redis_index(&procly, worktree)).collect();
    assert_eq!(indexes, [1, 2, 3]);
    assert_eq!(redis_index(&procly, &main), 0);
    let before = procly.procly(&["gc"]);
    assert_success(&before);
    assert!(!stdout(&before).contains("logical resource"), "{}", stdout(&before));
    // `app-a` is on a branch of its own, `app-b` on a Pinned branch, and `app-c` stays.
    remove_worktree(&main, &others[0]);
    remove_worktree(&main, &others[1]);

    let listed = procly.procly(&["gc"]);

    assert_success(&listed);
    let text = stdout(&listed);
    let resources: Vec<&str> = text
        .lines()
        .filter(|line| line.starts_with("logical resource"))
        .collect();
    assert_eq!(
        resources,
        [format!(
            "logical resource cache.main 1 (driver redis) of {}",
            others[0].display()
        )
        .as_str()]
    );
    let document = common::json_succeeded(&procly.procly(&["gc", "--json"]));
    let in_document: Vec<&serde_json::Value> = document["orphans"]
        .as_array()
        .expect("orphans is a list")
        .iter()
        .filter(|orphan| orphan["kind"] == "logical_resource")
        .collect();
    assert_eq!(
        in_document,
        [&serde_json::json!({
            "kind": "logical_resource",
            "service": "cache",
            "resource": "main",
            "name": "1",
            "driver": "redis",
            "repository": main.join(".git"),
            "worktree": others[0],
            "deleted": false,
            "error": null,
        })]
    );

    // The container never ran, so nothing can be dropped, and the index stays taken.
    let applied = procly.procly(&["gc", "--apply"]);

    assert_eq!(applied.status.code(), Some(6), "{}", stderr(&applied));
    assert!(
        stderr(&applied).contains(&format!(
            "procly: cannot delete logical resource cache.main 1 (driver redis) of {}: the container of service \
             cache does not run",
            others[0].display()
        )),
        "{}",
        stderr(&applied)
    );
    let next = git_worktree_add(&main, "app-d");
    fs::write(next.join("procly.toml"), &config).expect("write the config");
    write_driver_file(&next.join(".procly/drivers"), "redis", &redis_driver(16));
    assert_eq!(redis_index(&procly, &next), 4);
}

#[test]
fn docker_gc_apply_drops_the_logical_resources_of_a_removed_worktree_through_the_driver() {
    if !require_docker() {
        return;
    }
    let fixture = directory_fixture(DEV_AND_TEST);
    assert_success(&fixture.procly.procly_in(&fixture.main, &["up", "db"]));
    assert_success(&fixture.procly.procly_in(&fixture.other, &["up", "db"]));
    let kept = git_worktree_add(&fixture.main, "app-kept");
    assert_success(&fixture.procly.procly_in(&kept, &["up", "db"]));
    remove_worktree(&fixture.main, &fixture.other);
    let other = fixture.other.display();
    let dev = format!("logical resource db.dev app__app_other (driver dir) of {other}");
    let test = format!("logical resource db.test app_test__app_other (driver dir) of {other}");

    let listed = fixture.procly.procly(&["gc"]);

    assert_success(&listed);
    assert!(
        stdout(&listed).contains(&format!("{dev}\n{test}\n")),
        "{}",
        stdout(&listed)
    );
    assert!(!stdout(&listed).contains("app_kept"), "{}", stdout(&listed));
    assert_eq!(
        resources(&fixture.project),
        [
            "app",
            "app__app_kept",
            "app__app_other",
            "app_test",
            "app_test__app_kept",
            "app_test__app_other"
        ]
    );

    let applied = fixture.procly.procly(&["gc", "--apply"]);

    assert_success(&applied);
    assert!(
        stdout(&applied).contains(&format!("deleted {dev}\ndeleted {test}\n")),
        "{}",
        stdout(&applied)
    );
    // A Worktree git still lists keeps its resources, and the Main worktree its own.
    assert_eq!(
        resources(&fixture.project),
        ["app", "app__app_kept", "app_test", "app_test__app_kept"]
    );
    let log = fixture.procly.daemon_log();
    assert!(log.contains(&format!("gc deleted {dev}, requested by pid ")), "{log}");
    let again = fixture.procly.procly(&["gc"]);
    assert!(!stdout(&again).contains("logical resource"), "{}", stdout(&again));
}

#[test]
fn docker_gc_apply_frees_the_redis_index_of_a_removed_worktree_for_a_new_one() {
    if !require_docker() {
        return;
    }
    let api = "[services.db.resources.main]\n[services.api]\nrun = 'true'\n[services.api.env]\n\
               REDIS_URL = '{services.db.resources.main.url}'\n";
    let fixture = logical_fixture(redis_compose, "redis", api);
    // A container holding 2 indexes: the Main worktree and one other Worktree take them all.
    write_driver_file(&fixture.main.join(".procly/drivers"), "redis", &redis_driver(2));
    let fixture = commit_and_add_worktree(fixture);
    let index = |worktree: &Path| {
        let url = exported(&fixture.procly.procly_in(worktree, &["env", "api"]), "REDIS_URL");
        url.rsplit('/').next().expect("an index").to_owned()
    };
    let redis = |database: &str, command: &str| {
        fixture
            .project
            .exec(&format!("redis-cli -n {database} {command}"))
            .trim()
            .to_owned()
    };
    assert_success(&fixture.procly.procly_in(&fixture.main, &["up", "db"]));
    assert_success(&fixture.procly.procly_in(&fixture.other, &["up", "db"]));
    assert_eq!(index(&fixture.other), "1");
    assert_eq!(redis("1", "set session other-worktree"), "OK");
    let next = git_worktree_add(&fixture.main, "app-next");
    let exhausted = fixture.procly.procly_in(&next, &["up", "db"]);
    assert!(
        stderr(&exhausted).contains("all 2 indexes of service db are taken"),
        "{}",
        stderr(&exhausted)
    );
    remove_worktree(&fixture.main, &fixture.other);

    let applied = fixture.procly.procly(&["gc", "--apply"]);

    assert_success(&applied);
    assert!(
        stdout(&applied).contains(&format!(
            "deleted logical resource db.main 1 (driver redis) of {}\n",
            fixture.other.display()
        )),
        "{}",
        stdout(&applied)
    );
    assert_eq!(redis("1", "get session"), "");
    // The freed index is granted to the next Worktree, empty.
    assert_success(&fixture.procly.procly_in(&next, &["up", "db"]));
    assert_eq!(index(&next), "1");
}

#[test]
fn docker_a_drop_that_fails_is_reported_with_the_command_to_run_and_gc_goes_on() {
    if !require_docker() {
        return;
    }
    let fixture = logical_fixture(alpine_compose, "dir", DEV_AND_TEST);
    // The drop of the `dev` resource of the other Worktree fails, the one of its `test` resource works.
    let failing = DIRECTORY_DRIVER.replace(
        "drop = 'rm",
        "drop = 'test {resource} != app__app_other || { echo refused >&2; exit 3; } ; rm",
    );
    write_driver_file(&fixture.main.join(".procly/drivers"), "dir", &failing);
    let fixture = commit_and_add_worktree(fixture);
    assert_success(&fixture.procly.procly_in(&fixture.main, &["up", "db"]));
    assert_success(&fixture.procly.procly_in(&fixture.other, &["up", "db"]));
    remove_worktree(&fixture.main, &fixture.other);
    let other = fixture.other.display();

    let applied = fixture.procly.procly(&["gc", "--apply"]);

    assert_eq!(applied.status.code(), Some(6), "{}", stderr(&applied));
    assert!(
        stdout(&applied).contains(&format!(
            "deleted logical resource db.test app_test__app_other (driver dir) of {other}\n"
        )),
        "{}",
        stdout(&applied)
    );
    let stderr = stderr(&applied);
    assert!(
        stderr.contains(&format!(
            "procly: cannot delete logical resource db.dev app__app_other (driver dir) of {other}: "
        )),
        "{stderr}"
    );
    assert!(stderr.contains("refused"), "{stderr}");
    assert!(
        stderr.contains(&format!(
            "drop it by hand with `docker compose --project-name {} exec -T db sh -c 'test app__app_other != \
             app__app_other ||",
            fixture.project.name
        )),
        "{stderr}"
    );
    assert_eq!(resources(&fixture.project), ["app", "app__app_other", "app_test"]);
    // It stays an Orphan, which the next `gc --apply` tries again.
    let listed = fixture.procly.procly(&["gc"]);
    assert!(
        stdout(&listed).contains(&format!(
            "logical resource db.dev app__app_other (driver dir) of {other}\n"
        )),
        "{}",
        stdout(&listed)
    );
}
