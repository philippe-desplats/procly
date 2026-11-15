//! A tree that listens on the declared port of a Service instead of its Lease, as an app that reads its port from a
//! file of its own does in a secondary Worktree, is a Port mismatch at once, named per Service.

mod common;

use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::{Duration, Instant};

use common::{Procly, assert_success, free_port, git_repository, json_document, stderr, stdout, test_server};
use serde_json::{Value, json};

/// Far below the health timeout of 60 s the Services of these tests declare.
const FAST: Duration = Duration::from_secs(15);

fn write(dir: &Path, name: &str, content: &str) {
    std::fs::write(dir.join(name), content).expect("write the config file");
}

/// The Lease of `service` in the Worktree `dir`, as `procly env` prints it.
fn lease_of(procly: &Procly, dir: &Path, service: &str) -> u16 {
    let output = procly.procly_in(dir, &["env", service]);
    assert_success(&output);
    stdout(&output)
        .lines()
        .find_map(|line| line.strip_prefix("export PORT='"))
        .and_then(|port| port.trim_end_matches('\'').parse().ok())
        .unwrap_or_else(|| panic!("no Lease of {service} in {}", stdout(&output)))
}

/// A Repository with a secondary Worktree, both holding `config`, and a Range of its own so that the Leases of the
/// secondary Worktree differ from the declared ports. Returns the Main worktree and the secondary one.
fn worktrees(procly: &Procly, config: &str) -> (PathBuf, PathBuf) {
    procly.narrow_range();
    let main = git_repository(procly.scratch(), "app");
    write(&main, "procly.toml", config);
    let secondary = common::git_worktree_add(&main, "app-feature");
    write(&secondary, "procly.toml", config);
    (main, secondary)
}

/// Services `api` and `web` declaring `api` and `web`, and a Group `dev` covering them whose run is `run`.
fn group_config(api: u16, web: u16, run: &str) -> String {
    let server = test_server().display().to_string();
    format!(
        "[services.api]\nrun = '{server} --port {{ports.http}}'\nports = {{ http = {api} }}\n\
         [services.web]\nrun = '{server} --port {{ports.http}}'\nports = {{ http = {web} }}\n\
         [services.dev]\ncovers = ['api', 'web']\nrun = \"{run}\"\nhealth = {{ timeout = '60s' }}\n"
    )
}

/// The Group of 2 apps that read their ports from files of their own: one process listens on both declared ports.
fn fixed_group(api: u16, web: u16) -> String {
    group_config(
        api,
        web,
        &format!("exec {} --port {api} --also {web}", test_server().display()),
    )
}

/// A Service `web` declaring `web` whose run is `run`.
fn web_config(web: u16, run: &str) -> String {
    format!("[services.web]\nrun = '{run}'\nports = {{ http = {web} }}\nhealth = {{ timeout = '60s' }}\n")
}

/// Runs `procly up` with `args` in `dir`, and how long it took.
fn timed_up(procly: &Procly, dir: &Path, args: &[&str]) -> (Output, Duration) {
    let started = Instant::now();
    let output = procly.procly_in(dir, &[&["up"], args].concat());
    (output, started.elapsed())
}

fn assert_fast_mismatch(output: &Output, took: Duration) {
    assert_eq!(output.status.code(), Some(4), "stderr: {}", stderr(output));
    assert!(
        took < FAST,
        "a Port mismatch seen early fails at once, it took {took:?}\n{}",
        stderr(output)
    );
}

/// The row of `service` in the document of `procly status --json`.
fn status_row(procly: &Procly, dir: &Path, service: &str) -> Value {
    let output = procly.procly_in(dir, &["status", "--json"]);
    assert_success(&output);
    let document = json_document(&output);
    document["repositories"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|repository| repository["worktrees"].as_array().into_iter().flatten())
        .filter(|worktree| worktree["path"] == json!(dir))
        .flat_map(|worktree| worktree["services"].as_array().into_iter().flatten())
        .find(|row| row["service"] == json!(service))
        .cloned()
        .unwrap_or_else(|| panic!("no row of {service} in {document}"))
}

#[test]
fn a_group_whose_apps_listen_on_their_declared_ports_in_a_secondary_worktree_fails_at_once_naming_each_service() {
    let procly = Procly::new();
    let (api, web) = (free_port(), free_port());
    let (_, secondary) = worktrees(&procly, &fixed_group(api, web));
    let (api_lease, web_lease) = (
        lease_of(&procly, &secondary, "api"),
        lease_of(&procly, &secondary, "web"),
    );
    assert!(
        api_lease != api && web_lease != web,
        "the secondary Worktree is leased other ports"
    );

    let (output, took) = timed_up(&procly, &secondary, &["dev"]);

    assert_fast_mismatch(&output, took);
    let message = stderr(&output);
    let first = message.lines().next().unwrap_or_default();
    assert_eq!(
        first,
        format!(
            "procly: service dev listens on ports other than the Leases of the Services it covers (Port mismatch): \
             api on port {api} instead of its Lease, port {api_lease}, web on port {web} instead of its Lease, port \
             {web_lease}, its process tree was stopped, its recent output:"
        ),
    );

    let status = stdout(&procly.procly_in(&secondary, &["status"]));
    for fragment in [
        format!("api on port {api} instead of its Lease, port {api_lease}"),
        format!("web on port {web} instead of its Lease, port {web_lease}"),
    ] {
        assert!(status.contains(&fragment), "status names {fragment}\n{status}");
    }
    let services = json!([
        { "service": "api", "leased": api_lease, "seen": [api] },
        { "service": "web", "leased": web_lease, "seen": [web] },
    ]);
    let end = &status_row(&procly, &secondary, "dev")["end"];
    assert_eq!(end["kind"], json!("port_mismatch"));
    assert_eq!(end["services"], services);
    assert_eq!(end.get("unattributed"), None, "{end}");
    assert_eq!(
        (end["leased"].clone(), end["actual"].clone()),
        (json!(api_lease), json!(api)),
        "one Service's pair"
    );

    let (output, took) = timed_up(&procly, &secondary, &["--json", "dev"]);
    assert_fast_mismatch(&output, took);
    assert_eq!(json_document(&output)["end"]["services"], services);
}

#[test]
fn a_service_whose_run_ignores_port_and_listens_on_its_declared_port_fails_at_once() {
    let procly = Procly::new();
    let web = free_port();
    let server = test_server().display().to_string();
    let (_, secondary) = worktrees(&procly, &web_config(web, &format!("{server} --port {web}")));
    let lease = lease_of(&procly, &secondary, "web");

    let (output, took) = timed_up(&procly, &secondary, &["web"]);

    assert_fast_mismatch(&output, took);
    assert!(
        stderr(&output).starts_with(&format!(
            "procly: service web listens on port {web} instead of its Lease, port {lease} (Port mismatch), its \
             process tree was stopped"
        )),
        "{}",
        stderr(&output)
    );
    let services = json!([{ "service": "web", "leased": lease, "seen": [web] }]);
    assert_eq!(status_row(&procly, &secondary, "web")["end"]["services"], services);

    let started = Instant::now();
    let output = procly.procly_in(&secondary, &["restart", "--json", "web"]);
    assert_fast_mismatch(&output, started.elapsed());
    assert_eq!(json_document(&output)["end"]["services"], services);
}

#[test]
fn a_port_opened_before_the_lease_that_no_service_declares_is_no_early_mismatch() {
    let procly = Procly::new();
    let (web, extra) = (free_port(), free_port());
    let server = test_server().display().to_string();
    let config = web_config(
        web,
        &format!("{server} --port {{ports.http}} --also {extra} --after 1500"),
    );
    let (_, secondary) = worktrees(&procly, &config);

    let output = procly.procly_in(&secondary, &["up", "web"]);

    assert_success(&output);
    assert_eq!(status_row(&procly, &secondary, "web")["state"], json!("healthy"));
}

#[test]
fn a_group_whose_covered_app_opens_an_unrelated_port_before_its_lease_becomes_healthy() {
    let procly = Procly::new();
    let (api, web, extra) = (free_port(), free_port(), free_port());
    let server = test_server().display().to_string();
    let run =
        format!("{server} --port $PORT_API_HTTP --also {extra} --after 1500 & exec {server} --port $PORT_WEB_HTTP");
    let (_, secondary) = worktrees(&procly, &group_config(api, web, &run));

    let output = procly.procly_in(&secondary, &["up", "dev"]);

    assert_success(&output);
    assert_eq!(status_row(&procly, &secondary, "dev")["state"], json!("healthy"));
}

#[test]
fn in_the_main_worktree_apps_on_their_declared_ports_are_on_their_leases_and_healthy() {
    let procly = Procly::new();
    let (api, web) = (free_port(), free_port());
    let (main, _) = worktrees(&procly, &fixed_group(api, web));

    assert_success(&procly.procly_in(&main, &["up", "dev"]));
    assert_eq!(status_row(&procly, &main, "dev")["state"], json!("healthy"));
    assert_success(&procly.procly_in(&main, &["down", "dev"]));

    let other = free_port();
    let server = test_server().display().to_string();
    write(
        &main,
        "procly.toml",
        &web_config(other, &format!("{server} --port {other}")),
    );
    assert_success(&procly.procly_in(&main, &["up", "web"]));
    assert_eq!(status_row(&procly, &main, "web")["state"], json!("healthy"));
}
