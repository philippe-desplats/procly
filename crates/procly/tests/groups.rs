mod common;

use std::net::TcpStream;
use std::path::Path;
use std::time::Duration;

use common::{Procly, assert_success, eventually, free_port, git_repository, stderr, stdout, test_server};

/// Writes `content` to `name` at the root of the Worktree `dir`.
fn write(dir: &Path, name: &str, content: &str) {
    std::fs::write(dir.join(name), content).expect("write the config file");
}

fn listens(port: u16) -> bool {
    TcpStream::connect(("127.0.0.1", port)).is_ok()
}

/// A monorepo whose dev command `dev` runs the test servers of `web` and `api` in one shell, from the Leases it is
/// handed. It appends a line to `starts` each time it starts and records its environment in `seen`.
fn monorepo(web: u16, api: u16, starts: &Path, seen: &Path) -> String {
    let server = test_server();
    format!(
        "[services.web]\nrun = '{server} --port {web}'\nports = {{ http = {web} }}\n\
         [services.web.env]\nWEB_URL = 'http://localhost:{{services.web.ports.http}}'\n\
         [services.api]\nrun = '{server} --port {api}'\nports = {{ http = {api} }}\n\
         [services.api.env]\nAPI_URL = 'http://localhost:{{services.api.ports.http}}'\n\
         [services.dev]\ncovers = ['web', 'api']\n\
         run = \"echo start >> {starts}; echo \\\"$PROCLY_SERVICE $WEB_URL $API_URL $PORT_WEB_HTTP $PORT_API_HTTP\\\" > {seen}; \
         {server} --port $PORT_WEB_HTTP & exec {server} --port $PORT_API_HTTP\"\n",
        server = server.display(),
        starts = starts.display(),
        seen = seen.display(),
    )
}

fn start_count(starts: &Path) -> usize {
    std::fs::read_to_string(starts).map_or(0, |starts| starts.lines().count())
}

#[test]
fn a_group_runs_as_one_unit_with_the_leases_and_derived_variables_of_every_service_it_covers() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let (web, api) = (free_port(), free_port());
    let (starts, seen) = (procly.scratch().join("starts"), procly.scratch().join("seen"));
    write(&main, "procly.toml", &monorepo(web, api, &starts, &seen));

    let output = procly.procly_in(&main, &["up", "dev"]);

    assert_success(&output);
    assert_eq!(stdout(&output), "dev started\n");
    assert!(
        listens(web) && listens(api),
        "both covered Services listen once up returns"
    );
    assert_eq!(
        std::fs::read_to_string(&seen).unwrap(),
        format!("dev http://localhost:{web} http://localhost:{api} {web} {api}\n")
    );

    // One unit: stopping the Group stops both servers.
    assert_success(&procly.procly_in(&main, &["down", "dev"]));
    eventually("both Services are gone", || !listens(web) && !listens(api));
    assert_eq!(start_count(&starts), 1);
}

#[test]
fn a_start_request_for_a_covered_service_while_the_group_runs_answers_reused_via_the_group() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let (web, api) = (free_port(), free_port());
    let (starts, seen) = (procly.scratch().join("starts"), procly.scratch().join("seen"));
    write(&main, "procly.toml", &monorepo(web, api, &starts, &seen));
    assert_success(&procly.procly_in(&main, &["up", "dev"]));

    let output = procly.procly_in(&main, &["up", "web"]);

    assert_success(&output);
    assert_eq!(
        stdout(&output),
        format!("web reused (via dev) http://localhost:{web} port {web}\n")
    );
    assert_eq!(start_count(&starts), 1);
    // Nothing of its own was started: stopping the Group is all it takes to free the port.
    assert_success(&procly.procly_in(&main, &["down", "dev"]));
    eventually("the covered Service is gone", || !listens(web));
}

#[test]
fn a_group_and_a_service_it_covers_never_run_side_by_side() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let (web, api) = (free_port(), free_port());
    let (starts, seen) = (procly.scratch().join("starts"), procly.scratch().join("seen"));
    write(&main, "procly.toml", &monorepo(web, api, &starts, &seen));
    assert_success(&procly.procly_in(&main, &["up", "web"]));

    let refused = procly.procly_in(&main, &["up", "dev"]);

    assert_eq!(refused.status.code(), Some(125), "stderr: {}", stderr(&refused));
    let message = stderr(&refused);
    assert!(
        message.contains("dev") && message.contains("web") && message.contains("procly down"),
        "{message}"
    );
    assert_eq!(start_count(&starts), 0, "the Group was not started");
    assert!(!listens(api));

    // Once the covered Service is stopped, the Group starts.
    assert_success(&procly.procly_in(&main, &["down", "web"]));
    assert_success(&procly.procly_in(&main, &["up", "dev"]));
    assert_eq!(start_count(&starts), 1);
}

#[test]
fn stopping_a_covered_service_while_its_group_runs_points_to_the_group() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let (web, api) = (free_port(), free_port());
    let (starts, seen) = (procly.scratch().join("starts"), procly.scratch().join("seen"));
    write(&main, "procly.toml", &monorepo(web, api, &starts, &seen));
    assert_success(&procly.procly_in(&main, &["up", "dev"]));

    let output = procly.procly_in(&main, &["down", "web"]);

    assert_eq!(output.status.code(), Some(125), "stderr: {}", stderr(&output));
    assert!(stderr(&output).contains("procly down dev"), "{}", stderr(&output));
    assert!(listens(web), "the Group keeps running");
}

#[test]
fn up_without_arguments_starts_every_declared_service_respecting_needs() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let (web, api) = (free_port(), free_port());
    let server = test_server();
    write(
        &main,
        "procly.toml",
        &format!(
            "[services.web]\nrun = '{server} --port {web}'\nports = {{ http = {web} }}\nneeds = ['api']\n\
             [services.api]\nrun = '{server} --port {api}'\nports = {{ http = {api} }}\n",
            server = server.display()
        ),
    );

    let output = procly.procly_in(&main, &["up"]);

    assert_success(&output);
    assert_eq!(
        stdout(&output),
        format!("api started http://localhost:{api} port {api}\nweb started http://localhost:{web} port {web}\n")
    );
    let again = procly.procly_in(&main, &["up"]);
    assert_eq!(
        stdout(&again),
        format!("api reused http://localhost:{api} port {api}\nweb reused http://localhost:{web} port {web}\n")
    );
}

#[test]
fn up_without_arguments_starts_a_group_and_not_the_services_it_covers() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let (web, api) = (free_port(), free_port());
    let (starts, seen) = (procly.scratch().join("starts"), procly.scratch().join("seen"));
    write(&main, "procly.toml", &monorepo(web, api, &starts, &seen));

    let output = procly.procly_in(&main, &["up"]);

    assert_success(&output);
    assert_eq!(stdout(&output), "dev started\n");
    assert_eq!(start_count(&starts), 1);
    let covered = procly.procly_in(&main, &["up", "api"]);
    assert_eq!(
        stdout(&covered),
        format!("api reused (via dev) http://localhost:{api} port {api}\n")
    );
}

#[test]
fn up_without_arguments_fails_when_nothing_is_declared() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");

    let output = procly.procly_in(&main, &["up"]);

    assert_eq!(output.status.code(), Some(125), "stderr: {}", stderr(&output));
}

#[test]
fn a_group_is_healthy_only_once_every_service_it_covers_answers() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let (web, api) = (free_port(), free_port());
    let server = test_server();
    // The dev command serves `web` and forgets `api`.
    write(
        &main,
        "procly.toml",
        &format!(
            "[services.web]\nrun = 'x'\nports = {{ http = {web} }}\n\
             [services.api]\nrun = 'x'\nports = {{ http = {api} }}\n\
             [services.dev]\ncovers = ['web', 'api']\nrun = '{server} --port $PORT_WEB_HTTP'\n\
             health = {{ timeout = '2s' }}\n",
            server = server.display()
        ),
    );

    let output = procly.procly_in(&main, &["up", "dev"]);

    assert_eq!(output.status.code(), Some(124), "stderr: {}", stderr(&output));
    assert!(stderr(&output).contains("within 2s"), "{}", stderr(&output));
    eventually("the group is stopped", || !listens(web));
}

#[test]
fn env_prints_what_a_group_receives() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let (web, api) = (free_port(), free_port());
    let (starts, seen) = (procly.scratch().join("starts"), procly.scratch().join("seen"));
    write(&main, "procly.toml", &monorepo(web, api, &starts, &seen));

    let output = procly.procly_in(&main, &["env", "dev"]);

    assert_success(&output);
    assert_eq!(
        stdout(&output),
        format!(
            "export API_URL='http://localhost:{api}'\nexport PORT_API_HTTP='{api}'\nexport PORT_WEB_HTTP='{web}'\n\
             export PROCLY='1'\nexport PROCLY_SERVICE='dev'\nexport WEB_URL='http://localhost:{web}'\n"
        )
    );
    assert_eq!(start_count(&starts), 0, "env starts nothing");
}

#[test]
fn a_group_running_when_the_daemon_was_killed_is_reattached_and_still_covers_its_services() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let (web, api) = (free_port(), free_port());
    let (starts, seen) = (procly.scratch().join("starts"), procly.scratch().join("seen"));
    write(&main, "procly.toml", &monorepo(web, api, &starts, &seen));
    assert_success(&procly.procly_in(&main, &["up", "dev"]));

    procly.kill_daemon();
    let covered = procly.procly_in(&main, &["up", "web"]);

    assert_success(&covered);
    assert_eq!(
        stdout(&covered),
        format!("web reused (via dev) http://localhost:{web} port {web}\n")
    );
    let group = procly.procly_in(&main, &["up", "dev"]);
    assert_eq!(stdout(&group), "dev reused\n");
    assert_eq!(start_count(&starts), 1);
    assert_success(&procly.procly_in(&main, &["down", "dev"]));
    eventually("both Services are gone", || !listens(web) && !listens(api));
}

/// The port `procly up` printed at the end of its line.
fn port_of(output: &std::process::Output) -> u16 {
    assert_success(output);
    let line = stdout(output);
    line.trim_end()
        .rsplit(' ')
        .next()
        .and_then(|port| port.parse().ok())
        .unwrap_or_else(|| panic!("no port at the end of {line:?}"))
}

/// The configuration of a `web` Service and a Group `dev` that covers it, never stopped for being idle since the
/// clock jumps by days while it runs.
fn group_config() -> String {
    format!(
        "[services.web]\nrun = '{server} --port {{ports.http}}'\nports = {{ http = {port} }}\n\
         [services.dev]\ncovers = ['web']\nrun = '{server} --port $PORT_WEB_HTTP'\nidle = {{ stop = false }}\n",
        port = free_port(),
        server = test_server().display()
    )
}

/// A Worktree of `main` named `name` holding `config`.
fn worktree_with(main: &Path, name: &str, config: &str) -> std::path::PathBuf {
    let worktree = common::git_worktree_add(main, name);
    write(&worktree, "procly.toml", config);
    worktree
}

/// The Lease of `web` in `worktree`, read without starting anything.
fn lease_of_web(procly: &Procly, worktree: &Path) -> u16 {
    let output = procly.procly_in(worktree, &["env", "web"]);
    stdout(&output)
        .lines()
        .find_map(|line| line.strip_prefix("export PORT='"))
        .and_then(|port| port.trim_end_matches('\'').parse().ok())
        .expect("the Lease of web")
}

#[test]
fn the_leases_of_the_services_a_group_covers_do_not_expire_while_it_runs() {
    let procly = Procly::with_clock();
    procly.narrow_range();
    let main = git_repository(procly.scratch(), "app");
    let config = group_config();
    let long_running = worktree_with(&main, "app-long", &config);
    let leased = lease_of_web(&procly, &long_running);
    assert_success(&procly.procly_in(&long_running, &["up", "dev"]));

    // 31 days since the Lease was granted, and the Group runs.
    procly.advance_clock(Duration::from_hours(31 * 24));

    assert_ne!(
        port_of(&procly.procly_in(&worktree_with(&main, "app-other", &config), &["up", "web"])),
        leased
    );
}

#[test]
fn the_leases_of_the_services_a_group_covers_are_dated_again_once_it_ends() {
    let procly = Procly::with_clock();
    procly.narrow_range();
    let main = git_repository(procly.scratch(), "app");
    let config = group_config();
    let long_running = worktree_with(&main, "app-long", &config);
    let leased = lease_of_web(&procly, &long_running);
    assert_success(&procly.procly_in(&long_running, &["up", "dev"]));
    procly.advance_clock(Duration::from_hours(29 * 24));
    assert_success(&procly.procly_in(&long_running, &["down", "dev"]));

    // 31 days since the Lease was granted, 2 days since its Group ended.
    procly.advance_clock(Duration::from_hours(2 * 24));

    assert_ne!(
        port_of(&procly.procly_in(&worktree_with(&main, "app-other", &config), &["up", "web"])),
        leased
    );
}

#[test]
fn a_group_that_no_longer_listens_on_the_lease_of_a_covered_service_is_not_reattached() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let (web, api) = (free_port(), free_port());
    let (server, web_pid) = (test_server(), procly.scratch().join("web-pid"));
    write(
        &main,
        "procly.toml",
        &format!(
            "[services.web]\nrun = '{server} --port {web}'\nports = {{ http = {web} }}\n\
             [services.api]\nrun = '{server} --port {api}'\nports = {{ http = {api} }}\n\
             [services.dev]\ncovers = ['web', 'api']\n\
             run = '{server} --port $PORT_WEB_HTTP & echo $! > {web_pid}; exec {server} --port $PORT_API_HTTP'\n",
            server = server.display(),
            web_pid = web_pid.display(),
        ),
    );
    assert_success(&procly.procly_in(&main, &["up", "dev"]));
    procly.kill_daemon();
    // `web` dies while no Daemon watches, the leader of the Group and `api` live on.
    nix::sys::signal::kill(common::read_pid(&web_pid), nix::sys::signal::Signal::SIGKILL).expect("kill web");
    eventually("web is gone", || !listens(web));

    let output = procly.procly_in(&main, &["up", "api"]);

    assert_success(&output);
    // Not `reused (via dev)`: the Group was stopped, and `api` starts on its own.
    assert_eq!(
        stdout(&output),
        format!("api started http://localhost:{api} port {api}\n")
    );
    assert!(listens(api));
}
