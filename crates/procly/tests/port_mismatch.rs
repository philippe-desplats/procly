//! A tree that listens on the declared port of a Service instead of its Lease, as an app that reads its port from a
//! file of its own does in a secondary Worktree, is a Port mismatch at once, named per Service.

mod common;

use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{Procly, assert_success, free_port, git_repository, http_get, json_document, stderr, stdout, test_server};
use serde_json::{Value, json};

/// Far below the health timeout of 60 s the Services of these tests declare.
const FAST: Duration = Duration::from_secs(15);
/// How long the Daemon reads the tree of a Group again, once its Port mismatch is seen, for its other apps to bind.
const GRACE: Duration = Duration::from_secs(10);
/// What [`up_after_listening`] may see later than the Daemon does, a connection being tried every 20 ms.
const SEEN_LATE: Duration = Duration::from_millis(100);

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

/// Starts `procly up` with `args` in `dir`, without waiting for it.
fn spawn_up(procly: &Procly, dir: &Path, args: &[&str]) -> Child {
    procly
        .command()
        .current_dir(dir)
        .args([&["up"], args].concat())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the procly binary runs")
}

/// The moment `port` first accepts a connection while `up` runs, which is when the tree of the Service listens there.
fn first_listened(up: &mut Child, port: u16) -> Instant {
    let started = Instant::now();
    loop {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return Instant::now();
        }
        assert!(
            up.try_wait().expect("procly up can be waited for").is_none(),
            "procly up ended before port {port} was listened on"
        );
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "nothing listened on port {port}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Runs `procly up` with `args` in `dir`, and how long it took from the moment `port` first accepted a connection,
/// which is when the tree of the Service listens there.
fn up_after_listening(procly: &Procly, dir: &Path, args: &[&str], port: u16) -> (Output, Duration) {
    let mut up = spawn_up(procly, dir, args);
    let seen = first_listened(&mut up, port);
    let output = up.wait_with_output().expect("procly up ends");
    (output, seen.elapsed())
}

/// A listener on `port` that accepts every connection, as a Foreign process would, and records when it last
/// accepted one, which tells when the health check of the Lease on `port` last probed it.
struct Probed {
    /// When the first and the last probe came, if any did.
    probes: Arc<Mutex<Option<(Instant, Instant)>>>,
    done: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Probed {
    fn on(port: u16) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", port)).expect("hold the Lease");
        listener.set_nonblocking(true).expect("a listener that does not block");
        let (probes, done) = (Arc::new(Mutex::new(None)), Arc::new(AtomicBool::new(false)));
        let thread = {
            let (probes, done) = (Arc::clone(&probes), Arc::clone(&done));
            std::thread::spawn(move || {
                while !done.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok(_) => {
                            let now = Instant::now();
                            let mut probes = probes.lock().expect("the probe times");
                            let first = probes.map_or(now, |(first, _)| first);
                            *probes = Some((first, now));
                        }
                        Err(_) => std::thread::sleep(Duration::from_millis(5)),
                    }
                }
            })
        };
        Self {
            probes,
            done,
            thread: Some(thread),
        }
    }

    /// When the first probe came, if any did.
    fn first(&self) -> Option<Instant> {
        self.probes.lock().expect("the probe times").map(|(first, _)| first)
    }

    /// When the last probe came, if any did.
    fn last(&self) -> Option<Instant> {
        self.probes.lock().expect("the probe times").map(|(_, last)| last)
    }
}

impl Drop for Probed {
    fn drop(&mut self) {
        self.done.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Asserts that `up` exited with 4 once it had read the tree for the whole grace since the Port mismatch was seen,
/// `took` after it.
fn assert_mismatch_after_the_grace(output: &Output, took: Duration) {
    assert_eq!(output.status.code(), Some(4), "stderr: {}", stderr(output));
    assert!(
        took + SEEN_LATE >= GRACE,
        "the report says 10 s passed, it took {took:?}\n{}",
        stderr(output)
    );
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

#[test]
fn a_fixed_second_port_opened_before_the_lease_of_the_health_port_is_no_early_mismatch() {
    let procly = Procly::new();
    let (http, hmr) = (free_port(), free_port());
    let server = test_server().display().to_string();
    // The health port follows its Lease, the HMR port is written in the app and opens first.
    let config = format!(
        "[services.web]\nrun = '{server} --port {{ports.http}} --also {hmr} --after 1500'\n\
         ports = {{ http = {http}, hmr = {hmr} }}\nhealth = {{ port = 'http', timeout = '60s' }}\n"
    );
    let (_, secondary) = worktrees(&procly, &config);

    let output = procly.procly_in(&secondary, &["up", "web"]);

    assert_success(&output);
    assert_eq!(status_row(&procly, &secondary, "web")["state"], json!("healthy"));
}

#[test]
fn a_covered_service_on_its_lease_that_is_the_declared_port_of_another_is_no_early_mismatch() {
    let procly = Procly::new();
    let (api, web) = (free_port(), free_port());
    let server = test_server().display().to_string();
    // In a secondary Worktree `web` takes its Lease from its Pool, the port `api` declares, and binds it first.
    let config = format!(
        "[services.api]\nrun = '{server} --port {{ports.http}}'\nports = {{ http = {api} }}\n\
         [services.web]\nrun = '{server} --port {{ports.http}}'\nports = {{ http = {{ port = {web}, pool = [{api}] }} }}\n\
         [services.dev]\ncovers = ['api', 'web']\nhealth = {{ timeout = '60s' }}\n\
         run = \"exec {server} --port $PORT_API_HTTP --also $PORT_WEB_HTTP --after 1500\"\n"
    );
    let (_, secondary) = worktrees(&procly, &config);
    assert_eq!(
        lease_of(&procly, &secondary, "web"),
        api,
        "the Lease of web is the declared port of api"
    );

    let output = procly.procly_in(&secondary, &["up", "dev"]);

    assert_success(&output);
    assert_eq!(status_row(&procly, &secondary, "dev")["state"], json!("healthy"));
}

#[test]
fn apps_binding_their_declared_ports_seconds_apart_are_both_named() {
    let procly = Procly::new();
    let (api, web) = (free_port(), free_port());
    let server = test_server().display().to_string();
    let run = format!("{server} --port {api} & sleep 1.5; exec {server} --port {web}");
    let (_, secondary) = worktrees(&procly, &group_config(api, web, &run));
    let (api_lease, web_lease) = (
        lease_of(&procly, &secondary, "api"),
        lease_of(&procly, &secondary, "web"),
    );

    let (output, took) = timed_up(&procly, &secondary, &["dev"]);

    assert_fast_mismatch(&output, took);
    let message = stderr(&output);
    assert!(
        message.contains(&format!(
            "api on port {api} instead of its Lease, port {api_lease}, web on port {web} instead of its Lease, port \
             {web_lease}"
        )),
        "{message}"
    );
}

#[test]
fn an_app_binding_its_declared_port_4_s_after_the_other_is_named_with_its_port() {
    let procly = Procly::new();
    let (api, web) = (free_port(), free_port());
    let server = test_server().display().to_string();
    // The 4 s count from the moment `api` listens, whatever its own start took.
    let run = format!("{server} --port {api} --then sh -c 'sleep 4; exec {server} --port {web}'");
    let (_, secondary) = worktrees(&procly, &group_config(api, web, &run));
    let (api_lease, web_lease) = (
        lease_of(&procly, &secondary, "api"),
        lease_of(&procly, &secondary, "web"),
    );

    let (output, took) = up_after_listening(&procly, &secondary, &["dev"], api);

    assert_eq!(output.status.code(), Some(4), "stderr: {}", stderr(&output));
    // The wait stops once `web` listens, well before the grace is over.
    assert!(
        took < Duration::from_secs(8),
        "reported {took:?} after api listened\n{}",
        stderr(&output)
    );
    assert!(
        stderr(&output).contains(&format!(
            "api on port {api} instead of its Lease, port {api_lease}, web on port {web} instead of its Lease, port \
             {web_lease}, its process tree was stopped"
        )),
        "{}",
        stderr(&output)
    );
    assert_eq!(
        status_row(&procly, &secondary, "dev")["end"]["services"],
        json!([
            { "service": "api", "leased": api_lease, "seen": [api] },
            { "service": "web", "leased": web_lease, "seen": [web] },
        ])
    );
}

#[test]
fn a_covered_app_that_never_binds_is_told_as_silent_for_the_whole_grace() {
    let procly = Procly::new();
    let (api, web) = (free_port(), free_port());
    let run = format!("exec {} --port {api}", test_server().display());
    let (_, secondary) = worktrees(&procly, &group_config(api, web, &run));
    let (api_lease, web_lease) = (
        lease_of(&procly, &secondary, "api"),
        lease_of(&procly, &secondary, "web"),
    );

    let (output, took) = up_after_listening(&procly, &secondary, &["dev"], api);

    assert_mismatch_after_the_grace(&output, took);
    // The Daemon sees the mismatch once `api` listens, not before.
    assert!(took < FAST, "it took {took:?}\n{}", stderr(&output));
    let told = format!(
        "api on port {api} instead of its Lease, port {api_lease}, web did not listen on a port of its own within 10 \
         s, its Lease is port {web_lease}, its process tree was stopped"
    );
    assert!(stderr(&output).contains(&told), "{}", stderr(&output));
    let status = stdout(&procly.procly_in(&secondary, &["status"]));
    assert!(
        status.contains(&format!(
            "web did not listen on a port of its own within 10 s, its Lease is port {web_lease}"
        )),
        "{status}"
    );
    assert_eq!(
        status_row(&procly, &secondary, "dev")["end"]["services"],
        json!([
            { "service": "api", "leased": api_lease, "seen": [api] },
            { "service": "web", "leased": web_lease, "seen": [] },
        ])
    );
}

#[test]
fn the_health_deadline_does_not_cut_the_wait_for_the_other_apps_of_a_group() {
    let procly = Procly::new();
    let (api, web) = (free_port(), free_port());
    let server = test_server().display().to_string();
    // `web` binds 7 s after `api` listens, after the deadline and within the grace that started then.
    let run = format!("{server} --port {api} --then sh -c 'sleep 7; exec {server} --port {web}'");
    let config = group_config(api, web, &run).replace("timeout = '60s'", "timeout = '6s'");
    let (_, secondary) = worktrees(&procly, &config);
    let (api_lease, web_lease) = (
        lease_of(&procly, &secondary, "api"),
        lease_of(&procly, &secondary, "web"),
    );

    let (output, took) = timed_up(&procly, &secondary, &["dev"]);

    assert_fast_mismatch(&output, took);
    assert!(
        stderr(&output).contains(&format!(
            "api on port {api} instead of its Lease, port {api_lease}, web on port {web} instead of its Lease, port \
             {web_lease}"
        )),
        "{}",
        stderr(&output)
    );
}

#[test]
fn a_group_healthy_on_a_foreign_lease_while_a_covered_app_is_silent_reads_its_tree_for_the_whole_grace() {
    let procly = Procly::new();
    let (api, web, extra) = (free_port(), free_port(), free_port());
    // Nothing is judged early in the Main worktree, where each Lease is the declared port.
    let run = format!("exec {} --port $PORT_WEB_HTTP --also {extra}", test_server().display());
    let main = git_repository(procly.scratch(), "app");
    write(&main, "procly.toml", &group_config(api, web, &run));
    assert_eq!(
        (lease_of(&procly, &main, "api"), lease_of(&procly, &main, "web")),
        (api, web)
    );
    // A Foreign process answers the health check of `api`, whose app listens on no port of its own. It accepts, so that
    // its queue never fills with the probes of the health check and turns them down.
    let foreign = Probed::on(api);

    let (output, took) = up_after_listening(&procly, &main, &["dev"], web);

    assert_mismatch_after_the_grace(&output, took);
    assert!(took < FAST, "it took {took:?}\n{}", stderr(&output));
    assert!(
        stderr(&output).contains(&format!(
            "api did not listen on a port of its own within 10 s, its Lease is port {api}, web on its Lease, port \
             {web}, and port {extra} that no Service declares"
        )),
        "{}",
        stderr(&output)
    );
    drop(foreign);
}

#[test]
fn a_group_at_its_health_deadline_reads_its_tree_for_the_whole_grace() {
    let procly = Procly::new();
    let (api, web, extra) = (free_port(), free_port(), free_port());
    let server = test_server().display().to_string();
    // `web` binds its Lease 10 s after the tree first listens, after the deadline, `api` never does.
    let run = format!("{server} --port {extra} --then sh -c 'sleep 10; exec {server} --port $PORT_WEB_HTTP'");
    let config = group_config(api, web, &run).replace("timeout = '60s'", "timeout = '8s'");
    let main = git_repository(procly.scratch(), "app");
    write(&main, "procly.toml", &config);
    assert_eq!(lease_of(&procly, &main, "api"), api);
    // The health check probes the Lease of `api` until its deadline, the moment the Port mismatch is first seen.
    let probed = Probed::on(api);

    let output = spawn_up(&procly, &main, &["dev"])
        .wait_with_output()
        .expect("procly up ends");
    let ended = Instant::now();

    let deadline = probed.last().expect("the health check probed the Lease of api");
    assert_mismatch_after_the_grace(&output, ended - deadline);
    assert!(
        stderr(&output).contains(&format!(
            "api did not listen on a port of its own within 10 s, its Lease is port {api}, web on its Lease, port \
             {web}, and port {extra} that no Service declares"
        )),
        "{}",
        stderr(&output)
    );
}

#[test]
fn a_covered_app_that_listened_then_exited_during_the_grace_is_named_with_its_port() {
    let procly = Procly::new();
    let (api, web) = (free_port(), free_port());
    // The leader of the Group lives on once `api` exits, and `web` never binds.
    let run = format!("{} --port {api} & exec sleep 60", test_server().display());
    let (_, secondary) = worktrees(&procly, &group_config(api, web, &run));
    let (api_lease, web_lease) = (
        lease_of(&procly, &secondary, "api"),
        lease_of(&procly, &secondary, "web"),
    );

    let mut up = spawn_up(&procly, &secondary, &["dev"]);
    first_listened(&mut up, api);
    // Long enough for the Daemon to take the reading that decides on a loaded machine, well within the grace.
    std::thread::sleep(Duration::from_secs(3));
    http_get(api, "/exit/0");
    let output = up.wait_with_output().expect("procly up ends");

    assert_eq!(output.status.code(), Some(4), "stderr: {}", stderr(&output));
    assert!(
        stderr(&output).contains(&format!(
            "api on port {api} instead of its Lease, port {api_lease}, web did not listen on a port of its own within \
             10 s, its Lease is port {web_lease}"
        )),
        "{}",
        stderr(&output)
    );
    let end = &status_row(&procly, &secondary, "dev")["end"];
    assert_eq!(
        end["services"],
        json!([
            { "service": "api", "leased": api_lease, "seen": [api] },
            { "service": "web", "leased": web_lease, "seen": [] },
        ])
    );
    assert_eq!(
        (end["leased"].clone(), end["actual"].clone()),
        (json!(api_lease), json!(api))
    );
}

#[test]
fn the_wait_stops_once_each_covered_app_has_listened_even_one_that_exited_since() {
    let procly = Procly::new();
    let (api, web) = (free_port(), free_port());
    let server = test_server().display().to_string();
    // `web` binds once the test creates `go`, from a shell that outlives `api`, and the leader of the Group lives on.
    let go = procly.scratch().join("go");
    let run = format!(
        "(while [ ! -e {go} ]; do sleep 0.05; done; exec {server} --port {web}) & {server} --port {api} & exec sleep 60",
        go = go.display()
    );
    let (_, secondary) = worktrees(&procly, &group_config(api, web, &run));
    let (api_lease, web_lease) = (
        lease_of(&procly, &secondary, "api"),
        lease_of(&procly, &secondary, "web"),
    );

    let mut up = spawn_up(&procly, &secondary, &["dev"]);
    let seen = first_listened(&mut up, api);
    // Long enough for the Daemon to take the reading that decides on a loaded machine, before `web` binds.
    std::thread::sleep(Duration::from_secs(3));
    http_get(api, "/exit/0");
    while TcpStream::connect(("127.0.0.1", api)).is_ok() {
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_secs(4).saturating_sub(seen.elapsed()));
    std::fs::write(&go, "").expect("let web bind");
    let output = up.wait_with_output().expect("procly up ends");
    let took = seen.elapsed();

    assert_eq!(output.status.code(), Some(4), "stderr: {}", stderr(&output));
    // `api` listened earlier in the wait, so it ends once `web` listens, not at the end of the grace.
    assert!(
        took < Duration::from_secs(8),
        "reported {took:?} after api listened\n{}",
        stderr(&output)
    );
    assert!(
        stderr(&output).contains(&format!(
            "api on port {api} instead of its Lease, port {api_lease}, web on port {web} instead of its Lease, port \
             {web_lease}"
        )),
        "{}",
        stderr(&output)
    );
    assert_eq!(
        status_row(&procly, &secondary, "dev")["end"]["services"],
        json!([
            { "service": "api", "leased": api_lease, "seen": [api] },
            { "service": "web", "leased": web_lease, "seen": [web] },
        ])
    );
}

#[test]
fn a_service_alone_listening_on_no_port_of_its_own_is_reported_at_once() {
    let procly = Procly::new();
    let (web, extra) = (free_port(), free_port());
    let server = test_server().display().to_string();
    let main = git_repository(procly.scratch(), "app");
    write(
        &main,
        "procly.toml",
        &web_config(web, &format!("{server} --port {extra}")),
    );
    assert_eq!(lease_of(&procly, &main, "web"), web);
    // A Foreign process answers the health check, so that the tree is judged as soon as it listens. It accepts, so that
    // its queue never fills with the probes of the health check and turns them down.
    let foreign = Probed::on(web);

    let output = spawn_up(&procly, &main, &["web"])
        .wait_with_output()
        .expect("procly up ends");
    let ended = Instant::now();
    // Timed from the first probe of the health check, once the Service started: the start of the Daemon and of the
    // Service, which a loaded machine stretches, does not count. The port the tree listens on may be closed before a
    // test sees it, the report stopping the tree, so it cannot be the start.
    let first_probe = foreign
        .first()
        .expect("the health check probed the Lease of web before the Port mismatch was reported");
    let took = ended - first_probe;
    drop(foreign);

    assert_eq!(output.status.code(), Some(4), "stderr: {}", stderr(&output));
    assert!(
        stderr(&output).starts_with(&format!(
            "procly: service web listens on port {extra} instead of its Lease, port {web} (Port mismatch)"
        )),
        "{}",
        stderr(&output)
    );
    // Waiting for other apps would take the whole grace after the reading that decided, which follows the first probe.
    assert!(
        took < GRACE,
        "a Service alone waits for no other app, it took {took:?} from the first probe"
    );
    // The reading that decided is the one reported, with no later reading to stand in for it.
    let end = &status_row(&procly, &main, "web")["end"];
    assert_eq!(end["unattributed"], json!([extra]), "{end}");
    assert_eq!(
        end["services"],
        json!([{ "service": "web", "leased": web, "seen": [] }])
    );
}
