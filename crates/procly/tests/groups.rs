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

/// The Lease of `service` in the Worktree `dir`, as `procly env` prints it.
///
/// A test never assumes that the Main worktree got the port it declared: `free_port` releases its port at once, and
/// another process can take it before the Daemon checks it with a bind, which sends the Lease to the Range. The test
/// reads the port it was given instead, once the Services it asks about run, since a running Service keeps its Lease.
fn lease_of(procly: &Procly, dir: &Path, service: &str) -> u16 {
    let output = procly.procly_in(dir, &["env", service]);
    assert_success(&output);
    stdout(&output)
        .lines()
        .find_map(|line| line.strip_prefix("export PORT='"))
        .and_then(|port| port.trim_end_matches('\'').parse().ok())
        .unwrap_or_else(|| panic!("no Lease of {service} in {}", stdout(&output)))
}

/// A monorepo whose dev command `dev` runs the test servers of `web` and `api` in one shell, from the Leases it is
/// handed. It appends a line to `starts` each time it starts and records its environment in `seen`. `web` and `api`
/// run on their own on their Lease, `web` and `api` being the ports they declare.
fn monorepo(web: u16, api: u16, starts: &Path, seen: &Path) -> String {
    let server = test_server();
    format!(
        "[services.web]\nrun = '{server} --port {{ports.http}}'\nports = {{ http = {web} }}\n\
         [services.web.env]\nWEB_URL = 'http://localhost:{{services.web.ports.http}}'\n\
         [services.api]\nrun = '{server} --port {{ports.http}}'\nports = {{ http = {api} }}\n\
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

/// A monorepo whose Service `web` appends a line to `starts` each time it starts, and which a Group `dev` covers.
/// Nothing in these tests starts the Group.
fn covered_web(web: u16, starts: &Path) -> String {
    let server = test_server();
    format!(
        "[services.web]\nrun = \"echo start >> {starts}; exec {server} --port {{ports.http}}\"\n\
         ports = {{ http = {web} }}\n\
         [services.dev]\ncovers = ['web']\nrun = '{server} --port $PORT_WEB_HTTP'\n",
        server = server.display(),
        starts = starts.display(),
    )
}

/// The command lines of every process on the machine, one per line, as `ps` prints them.
fn process_table() -> String {
    let output = std::process::Command::new("ps")
        .args(["-A", "-o", "command="])
        .output()
        .expect("run ps");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// How many test servers listen for `port` on their command line, the instances of `web` that run.
fn instances_on(port: u16) -> usize {
    let wanted = format!("--port {port}");
    process_table()
        .lines()
        .filter(|line| line.contains("procly-test-server") && line.contains(&wanted))
        .count()
}

/// How many writers of the output files of the Daemon of `procly` run.
fn writers_of(procly: &Procly) -> usize {
    let state = procly.state_dir().display().to_string();
    process_table()
        .lines()
        .filter(|line| line.contains("output-file") && line.contains(&state))
        .count()
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
    let (web, api) = (lease_of(&procly, &main, "web"), lease_of(&procly, &main, "api"));
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
fn a_group_runs_on_the_leases_it_is_given_when_the_ports_its_services_declare_are_taken() {
    let procly = Procly::new();
    procly.narrow_range();
    let main = git_repository(procly.scratch(), "app");
    // Something else already listens on the declared ports, which sends the Leases of the Main worktree to the Range.
    let taken: Vec<_> = (0..2)
        .map(|_| std::net::TcpListener::bind(("127.0.0.1", 0)).expect("hold a port"))
        .collect();
    let [web, api] = [0, 1].map(|i| taken[i].local_addr().expect("read the port").port());
    let (starts, seen) = (procly.scratch().join("starts"), procly.scratch().join("seen"));
    write(&main, "procly.toml", &monorepo(web, api, &starts, &seen));

    assert_success(&procly.procly_in(&main, &["up", "dev"]));

    let (leased_web, leased_api) = (lease_of(&procly, &main, "web"), lease_of(&procly, &main, "api"));
    assert!(![web, api].contains(&leased_web) && ![web, api].contains(&leased_api));
    assert!(listens(leased_web) && listens(leased_api));
    assert_eq!(
        std::fs::read_to_string(&seen).unwrap(),
        format!("dev http://localhost:{leased_web} http://localhost:{leased_api} {leased_web} {leased_api}\n")
    );
    let reused = procly.procly_in(&main, &["up", "web"]);
    assert_eq!(
        stdout(&reused),
        format!("web reused (via dev) http://localhost:{leased_web} port {leased_web}\n")
    );
}

#[test]
fn a_start_request_for_a_covered_service_while_the_group_runs_answers_reused_via_the_group() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let (web, api) = (free_port(), free_port());
    let (starts, seen) = (procly.scratch().join("starts"), procly.scratch().join("seen"));
    write(&main, "procly.toml", &monorepo(web, api, &starts, &seen));
    assert_success(&procly.procly_in(&main, &["up", "dev"]));
    let web = lease_of(&procly, &main, "web");

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
fn a_second_start_request_for_a_covered_service_running_on_its_own_answers_reused_and_starts_nothing() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let starts = procly.scratch().join("starts");
    write(&main, "procly.toml", &covered_web(free_port(), &starts));

    let first = procly.procly_in(&main, &["up", "web"]);
    assert_success(&first);
    let web = lease_of(&procly, &main, "web");
    let second = procly.procly_in(&main, &["up", "web"]);

    assert_eq!(
        stdout(&first),
        format!("web started http://localhost:{web} port {web}\n")
    );
    assert!(second.status.success(), "{}", common::failure_message(&second));
    assert_eq!(
        stdout(&second),
        format!("web reused http://localhost:{web} port {web}\n")
    );
    assert_eq!(start_count(&starts), 1, "web was started once");
    assert_eq!(instances_on(web), 1, "one instance of web runs");
}

#[test]
fn down_stops_a_covered_service_that_a_second_start_request_found_running() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let starts = procly.scratch().join("starts");
    write(&main, "procly.toml", &covered_web(free_port(), &starts));
    assert_success(&procly.procly_in(&main, &["up", "web"]));
    let web = lease_of(&procly, &main, "web");
    // The second request is the one that used to start a duplicate, whatever it answered.
    let _ = procly.procly_in(&main, &["up", "web"]);

    let down = procly.procly_in(&main, &["down"]);

    assert_success(&down);
    assert_eq!(stdout(&down), "web stopped\n");
    eventually("web to stop listening", || !listens(web));
    assert_eq!(instances_on(web), 0, "no instance of web is left");
}

#[test]
fn stopping_the_daemon_leaves_no_process_nor_writer_of_a_covered_service_started_twice() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let starts = procly.scratch().join("starts");
    write(&main, "procly.toml", &covered_web(free_port(), &starts));
    assert_success(&procly.procly_in(&main, &["up", "web"]));
    let web = lease_of(&procly, &main, "web");
    let _ = procly.procly_in(&main, &["up", "web"]);
    assert_eq!(
        writers_of(&procly),
        1,
        "web has the writer of its output before the Daemon stops"
    );

    assert_success(&procly.procly(&["daemon", "stop"]));

    eventually("no instance of web to be left", || instances_on(web) == 0);
    eventually("no writer to be left", || writers_of(&procly) == 0);
}

#[test]
fn a_group_and_a_service_it_covers_never_run_side_by_side() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let (web, api) = (free_port(), free_port());
    let (starts, seen) = (procly.scratch().join("starts"), procly.scratch().join("seen"));
    write(&main, "procly.toml", &monorepo(web, api, &starts, &seen));
    assert_success(&procly.procly_in(&main, &["up", "web"]));
    let api = lease_of(&procly, &main, "api");

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

/// The names of the Services `procly status --json` lists for the Worktree `dir`, in the order it lists them.
fn listed_services(procly: &Procly, dir: &Path) -> Vec<String> {
    let output = procly.procly_in(dir, &["status", "--json"]);
    assert_success(&output);
    let status: serde_json::Value = serde_json::from_str(&stdout(&output)).expect("a JSON status");
    status["repositories"][0]["worktrees"][0]["services"]
        .as_array()
        .expect("a list of Services")
        .iter()
        .map(|service| service["service"].as_str().expect("a Service name").to_owned())
        .collect()
}

#[test]
fn a_group_starting_makes_the_daemon_forget_the_ended_rows_of_the_services_it_covers() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let (web, api) = (free_port(), free_port());
    let (starts, seen) = (procly.scratch().join("starts"), procly.scratch().join("seen"));
    write(&main, "procly.toml", &monorepo(web, api, &starts, &seen));
    assert_success(&procly.procly_in(&main, &["up", "web"]));
    assert_success(&procly.procly_in(&main, &["down", "web"]));
    assert_eq!(
        listed_services(&procly, &main),
        ["web"],
        "the Service that ended alone keeps its ended row until a Group stands for it"
    );

    assert_success(&procly.procly_in(&main, &["up", "dev"]));

    assert_eq!(
        listed_services(&procly, &main),
        ["dev"],
        "the Group is listed and no row of a Service it covers"
    );
}

#[test]
fn a_group_starting_leaves_the_ended_rows_of_the_services_it_does_not_cover() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let (web, api, worker) = (free_port(), free_port(), free_port());
    let (starts, seen) = (procly.scratch().join("starts"), procly.scratch().join("seen"));
    let config = format!(
        "{}[services.worker]\nrun = '{} --port {{ports.http}}'\nports = {{ http = {worker} }}\n",
        monorepo(web, api, &starts, &seen),
        test_server().display(),
    );
    write(&main, "procly.toml", &config);
    assert_success(&procly.procly_in(&main, &["up", "worker"]));
    assert_success(&procly.procly_in(&main, &["down", "worker"]));

    assert_success(&procly.procly_in(&main, &["up", "dev"]));

    assert_eq!(listed_services(&procly, &main), ["dev", "worker"]);
}

#[test]
fn stopping_a_covered_service_while_its_group_runs_points_to_the_group() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let (web, api) = (free_port(), free_port());
    let (starts, seen) = (procly.scratch().join("starts"), procly.scratch().join("seen"));
    write(&main, "procly.toml", &monorepo(web, api, &starts, &seen));
    assert_success(&procly.procly_in(&main, &["up", "dev"]));
    let web = lease_of(&procly, &main, "web");

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
            "[services.web]\nrun = '{server} --port {{ports.http}}'\nports = {{ http = {web} }}\nneeds = ['api']\n\
             [services.api]\nrun = '{server} --port {{ports.http}}'\nports = {{ http = {api} }}\n",
            server = server.display()
        ),
    );

    let output = procly.procly_in(&main, &["up"]);

    assert_success(&output);
    let (web, api) = (lease_of(&procly, &main, "web"), lease_of(&procly, &main, "api"));
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
    let api = lease_of(&procly, &main, "api");
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
    let web = lease_of(&procly, &main, "web");
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
    // Granted by the `env` above, and kept: a Lease stays as it is until a Service starts on it.
    let (web, api) = (lease_of(&procly, &main, "web"), lease_of(&procly, &main, "api"));
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
    let (web, api) = (lease_of(&procly, &main, "web"), lease_of(&procly, &main, "api"));

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

#[test]
fn the_leases_of_the_services_a_group_covers_do_not_expire_while_it_runs() {
    let procly = Procly::with_clock();
    procly.narrow_range();
    let main = git_repository(procly.scratch(), "app");
    let config = group_config();
    let long_running = worktree_with(&main, "app-long", &config);
    let leased = lease_of(&procly, &long_running, "web");
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
    let leased = lease_of(&procly, &long_running, "web");
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
            "[services.web]\nrun = '{server} --port {{ports.http}}'\nports = {{ http = {web} }}\n\
             [services.api]\nrun = '{server} --port {{ports.http}}'\nports = {{ http = {api} }}\n\
             [services.dev]\ncovers = ['web', 'api']\n\
             run = '{server} --port $PORT_WEB_HTTP & echo $! > {web_pid}; exec {server} --port $PORT_API_HTTP'\n",
            server = server.display(),
            web_pid = web_pid.display(),
        ),
    );
    assert_success(&procly.procly_in(&main, &["up", "dev"]));
    let web = lease_of(&procly, &main, "web");
    procly.kill_daemon();
    // `web` dies while no Daemon watches, the leader of the Group and `api` live on.
    nix::sys::signal::kill(common::read_pid(&web_pid), nix::sys::signal::Signal::SIGKILL).expect("kill web");
    eventually("web is gone", || !listens(web));

    let output = procly.procly_in(&main, &["up", "api"]);

    // Not `reused (via dev)`: the Group was stopped, and `api` starts on its own, on the Lease it gets now.
    let api = port_of(&output);
    assert_eq!(
        stdout(&output),
        format!("api started http://localhost:{api} port {api}\n")
    );
    assert!(listens(api));
}

#[test]
fn logs_of_a_service_covered_by_a_running_group_print_the_output_of_the_group() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let (starts, seen) = (procly.scratch().join("starts"), procly.scratch().join("seen"));
    write(
        &main,
        "procly.toml",
        &monorepo(free_port(), free_port(), &starts, &seen),
    );
    assert_success(&procly.procly_in(&main, &["up", "dev"]));
    let (web, api) = (lease_of(&procly, &main, "web"), lease_of(&procly, &main, "api"));

    eventually("the output of both servers of the group", || {
        let logs = stdout(&procly.procly_in(&main, &["logs", "dev"]));
        logs.contains(&format!("listening on {web}\n")) && logs.contains(&format!("listening on {api}\n"))
    });
    let logs = procly.procly_in(&main, &["logs", "web"]);

    assert_success(&logs);
    assert_eq!(stdout(&logs), stdout(&procly.procly_in(&main, &["logs", "dev"])));
}

#[test]
fn a_group_that_covers_one_service_is_reached_through_it_and_up_starts_the_others_on_their_own() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let (web, api) = (free_port(), free_port());
    let server = test_server();
    // What `procly init` proposes for a root script that starts only `web` besides a third-party process.
    write(
        &main,
        "procly.toml",
        &format!(
            "[services.web]\nrun = '{server} --port {{ports.http}}'\nports = {{ http = {web} }}\n\
             [services.api]\nrun = '{server} --port {{ports.http}}'\nports = {{ http = {api} }}\n\
             [services.dev]\ncovers = ['web']\nrun = '{server} --port $PORT_WEB_HTTP'\n",
            server = server.display()
        ),
    );

    let output = procly.procly_in(&main, &["up"]);

    assert_success(&output);
    let (web, api) = (lease_of(&procly, &main, "web"), lease_of(&procly, &main, "api"));
    // The Group stands for `web`, and `api` starts on its own.
    assert_eq!(
        stdout(&output),
        format!("api started http://localhost:{api} port {api}\ndev started\n")
    );
    let through_the_group = procly.procly_in(&main, &["up", "web"]);
    assert_eq!(
        stdout(&through_the_group),
        format!("web reused (via dev) http://localhost:{web} port {web}\n")
    );
    let on_its_own = procly.procly_in(&main, &["up", "api"]);
    assert_eq!(
        stdout(&on_its_own),
        format!("api reused http://localhost:{api} port {api}\n")
    );
    assert!(listens(web) && listens(api));
}

/// The port of the Lease of `service` in the Lease document, read without granting anything.
fn stored_lease(procly: &Procly, service: &str) -> u64 {
    let text = std::fs::read_to_string(procly.state_dir().join("leases.json")).expect("read the Lease document");
    let document: serde_json::Value = serde_json::from_str(&text).expect("a JSON Lease document");
    document["leases"]
        .as_array()
        .and_then(|leases| leases.iter().find(|lease| lease["service"] == service))
        .and_then(|lease| lease["port"].as_u64())
        .unwrap_or_else(|| panic!("no Lease of {service} in {text}"))
}

#[test]
fn a_group_whose_covered_service_cannot_move_moves_no_lease_of_the_others() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let (web, api) = (free_port(), free_port());
    let (starts, seen) = (procly.scratch().join("starts"), procly.scratch().join("seen"));
    write(&main, "procly.toml", &monorepo(web, api, &starts, &seen));
    assert_success(&procly.procly_in(&main, &["up", "dev"]));
    assert_success(&procly.procly_in(&main, &["down", "dev"]));
    // web, granted first, could move, api, granted next, cannot.
    let (new_web, new_api) = (free_port(), free_port());
    write(&main, "procly.toml", &monorepo(new_web, new_api, &starts, &seen));
    let foreign = std::net::TcpListener::bind(("127.0.0.1", new_api)).expect("hold the new declared port of api");

    let refused = procly.procly_in(&main, &["up", "dev"]);

    assert_eq!(refused.status.code(), Some(125), "{}", stderr(&refused));
    assert!(
        stderr(&refused).contains(&format!("the declared port {new_api} of Port name http of service api")),
        "{}",
        stderr(&refused)
    );
    assert_eq!(stored_lease(&procly, "web"), u64::from(web));
    assert_eq!(stored_lease(&procly, "api"), u64::from(api));
    drop(foreign);
}

#[test]
fn restart_of_a_group_whose_declarations_were_crossed_while_it_ran_moves_each_lease_to_its_declared_port() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let (first, second) = (free_port(), free_port());
    let (starts, seen) = (procly.scratch().join("starts"), procly.scratch().join("seen"));
    write(&main, "procly.toml", &monorepo(first, second, &starts, &seen));
    assert_success(&procly.procly_in(&main, &["up", "dev"]));
    // Swapped while the Group runs, each port is held by its own tree.
    write(&main, "procly.toml", &monorepo(second, first, &starts, &seen));

    let restarted = procly.procly_in(&main, &["restart", "dev"]);

    assert_success(&restarted);
    assert_eq!(stored_lease(&procly, "web"), u64::from(second));
    assert_eq!(stored_lease(&procly, "api"), u64::from(first));
    assert!(listens(first) && listens(second));
}
