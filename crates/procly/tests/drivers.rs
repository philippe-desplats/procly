//! Logical resources of Compose services, created by Drivers inside one shared container (ADR 0005). The tests
//! without the `docker_` prefix need no container: names and URLs are worked out from the configuration alone.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{Procly, assert_success, free_port, git_repository, git_worktree_add, stderr, stdout, test_server};

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

/// A Repository `app` with a second Worktree `app-other` (slug `app_other`, so its resources end in `_app_other`), and the port of its database.
fn repository(procly: &Procly, driver: &str) -> (PathBuf, PathBuf, u16) {
    let main = git_repository(procly.scratch(), "app");
    let other = git_worktree_add(&main, "app-other");
    let port = free_port();
    fs::write(main.join("procly.toml"), config(port, driver)).expect("write the config");
    fs::write(other.join("procly.toml"), config(port, driver)).expect("write the config");
    (main, other, port)
}

fn write_driver(dir: &Path, name: &str, url: &str) {
    fs::create_dir_all(dir).expect("create the Drivers directory");
    fs::write(
        dir.join(format!("{name}.toml")),
        format!("url = '{url}'\ncheck = 'true'\ncreate = 'true'\ndrop = 'true'\n"),
    )
    .expect("write the Driver");
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
    assert_eq!(exported(&in_other, "DATABASE_URL"), url("app_app_other"));
    assert_eq!(exported(&in_other, "TEST_DATABASE_URL"), url("app_test_app_other"));
    assert_eq!(exported(&in_other, "DATABASE_NAME"), "app_app_other");
}

#[test]
fn the_connection_urls_are_mirrored_in_the_env_file_of_the_worktree() {
    let procly = Procly::new();
    let (_, other, port) = repository(&procly, "postgres");

    assert_success(&procly.procly_in(&other, &["up", "api"]));

    let env_file = fs::read_to_string(other.join(".env.procly")).expect("read the Env file");
    assert!(
        env_file.contains(&format!(
            "DATABASE_URL=postgresql://postgres:postgres@localhost:{port}/app_app_other\n"
        )),
        "{env_file}"
    );
    assert!(env_file.contains("/app_test_app_other\n"), "{env_file}");
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
