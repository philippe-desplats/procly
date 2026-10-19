mod common;

use std::net::TcpStream;
use std::path::Path;

use common::{Procly, assert_success, free_port, git_repository, stdout, test_server};

/// Writes `content` to `name` at the root of the Worktree `dir`.
fn write(dir: &Path, name: &str, content: &str) {
    std::fs::write(dir.join(name), content).expect("write the config file");
}

/// A `procly.toml` declaring one Service `web` that runs the test server on `port`.
fn web_on(port: u16) -> String {
    format!(
        "[services.web]\nrun = \"{} --port {port}\"\nports = {{ http = {port} }}\n",
        test_server().display()
    )
}

#[test]
fn up_starts_a_declared_service_waits_for_health_and_prints_its_state_url_and_port() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let port = free_port();
    write(&main, "procly.toml", &web_on(port));

    let output = procly.procly_in(&main, &["up", "web"]);

    assert_success(&output);
    assert_eq!(
        stdout(&output),
        format!("web started http://localhost:{port} port {port}\n")
    );
    // Healthy means listening already, without any retry on our side.
    TcpStream::connect(("127.0.0.1", port)).expect("the service listens once up returns");
}

/// A `procly.toml` declaring `web`, which appends a line to `starts` each time a process starts.
fn counted_web_on(port: u16, starts: &Path) -> String {
    format!(
        "[services.web]\nrun = 'echo start >> {}; exec {} --port {port}'\nports = {{ http = {port} }}\n",
        starts.display(),
        test_server().display()
    )
}

fn start_count(starts: &Path) -> usize {
    std::fs::read_to_string(starts).map_or(0, |starts| starts.lines().count())
}

#[test]
fn a_second_up_answers_reused_without_starting_a_process() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let port = free_port();
    let starts = procly.scratch().join("starts");
    write(&main, "procly.toml", &counted_web_on(port, &starts));

    assert_success(&procly.procly_in(&main, &["up", "web"]));
    let output = procly.procly_in(&main, &["up", "web"]);

    assert_success(&output);
    assert_eq!(
        stdout(&output),
        format!("web reused http://localhost:{port} port {port}\n")
    );
    assert_eq!(start_count(&starts), 1);
}

#[test]
fn concurrent_ups_start_the_service_once() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let port = free_port();
    let starts = procly.scratch().join("starts");
    write(&main, "procly.toml", &counted_web_on(port, &starts));

    let ups: Vec<_> = (0..4)
        .map(|_| {
            procly
                .command()
                .current_dir(&main)
                .args(["up", "web"])
                .stdout(std::process::Stdio::piped())
                .spawn()
                .expect("procly up starts")
        })
        .collect();
    let mut outcomes: Vec<String> = ups
        .into_iter()
        .map(|up| {
            let output = up.wait_with_output().expect("procly up ends");
            assert_success(&output);
            stdout(&output).split(' ').nth(1).expect("an outcome").to_owned()
        })
        .collect();
    outcomes.sort();

    assert_eq!(outcomes, ["reused", "reused", "reused", "started"]);
    assert_eq!(start_count(&starts), 1);
}

#[test]
fn the_versioned_config_overrides_the_user_settings() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let starts = procly.scratch().join("starts");
    std::fs::write(
        procly.config_dir().join("config.toml"),
        "[settings]\nhealth_timeout = '1h'\n",
    )
    .unwrap();
    let versioned = deaf_web_on(free_port(), &starts, "[settings]\nhealth_timeout = '1s'\n");
    write(&main, "procly.toml", &versioned);

    let output = procly.procly_in(&main, &["up", "web"]);

    assert_eq!(output.status.code(), Some(124), "stderr: {}", common::stderr(&output));
    assert!(
        common::stderr(&output).contains("within 1s"),
        "{}",
        common::stderr(&output)
    );
}

#[test]
fn the_local_config_overrides_the_versioned_one_key_by_key() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let port = free_port();
    // The versioned file runs the server on the right port but declares a wrong one, the local file fixes it.
    let versioned = format!(
        "[services.web]\nrun = '{} --port {port}'\nports = {{ http = 2 }}\n",
        test_server().display()
    );
    write(&main, "procly.toml", &versioned);
    write(
        &main,
        "procly.local.toml",
        &format!("[services.web.ports]\nhttp = {port}\n"),
    );

    let output = procly.procly_in(&main, &["up", "web"]);

    assert_success(&output);
    assert_eq!(
        stdout(&output),
        format!("web started http://localhost:{port} port {port}\n")
    );
}

#[test]
fn the_user_settings_hold_settings_only() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    write(&main, "procly.toml", &web_on(free_port()));
    std::fs::write(
        procly.config_dir().join("config.toml"),
        "[services.mine]\nrun = 'true'\n",
    )
    .unwrap();

    let output = procly.procly_in(&main, &["up", "web"]);

    assert_eq!(output.status.code(), Some(125));
    assert!(
        common::stderr(&output).contains("holds only [settings]"),
        "{}",
        common::stderr(&output)
    );
}

/// A `procly.toml` declaring `web`, which records its pid in `starts` and never listens on `port`.
fn deaf_web_on(port: u16, starts: &Path, extra: &str) -> String {
    format!(
        "[services.web]\nrun = 'echo $$ >> {}; exec sleep 60'\nports = {{ http = {port} }}\n{extra}",
        starts.display()
    )
}

fn pids(starts: &Path) -> Vec<nix::unistd::Pid> {
    std::fs::read_to_string(starts)
        .unwrap_or_default()
        .lines()
        .map(|pid| nix::unistd::Pid::from_raw(pid.parse().expect("a pid")))
        .collect()
}

#[test]
fn up_exits_124_and_stops_the_service_when_health_never_comes() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let starts = procly.scratch().join("starts");
    let health = "[services.web.health]\ntimeout = '1s'\n";
    write(&main, "procly.toml", &deaf_web_on(free_port(), &starts, health));

    let output = procly.procly_in(&main, &["up", "web"]);

    assert_eq!(output.status.code(), Some(124), "stderr: {}", common::stderr(&output));
    assert_eq!(stdout(&output), "");
    assert!(
        common::stderr(&output).contains("web did not become healthy within 1s"),
        "stderr: {}",
        common::stderr(&output)
    );
    common::wait_until_gone(pids(&starts)[0]);

    // The stopped Service is not reused, the next up starts it again.
    let output = procly.procly_in(&main, &["up", "web"]);
    assert_eq!(output.status.code(), Some(124));
    assert_eq!(pids(&starts).len(), 2);
}

#[test]
fn the_health_timeout_defaults_to_the_one_in_the_user_settings() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let starts = procly.scratch().join("starts");
    std::fs::write(
        procly.config_dir().join("config.toml"),
        "[settings]\nhealth_timeout = '1s'\n",
    )
    .unwrap();
    write(&main, "procly.toml", &deaf_web_on(free_port(), &starts, ""));

    let output = procly.procly_in(&main, &["up", "web"]);

    assert_eq!(output.status.code(), Some(124), "stderr: {}", common::stderr(&output));
    assert!(
        common::stderr(&output).contains("within 1s"),
        "stderr: {}",
        common::stderr(&output)
    );
}

#[test]
fn up_exits_3_with_the_recent_output_when_the_service_dies_before_it_is_healthy() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let port = free_port();
    let config = format!("[services.web]\nrun = 'echo cannot bind >&2; exit 7'\nports = {{ http = {port} }}\n");
    write(&main, "procly.toml", &config);

    let start = std::time::Instant::now();
    let output = procly.procly_in(&main, &["up", "web"]);

    assert_eq!(output.status.code(), Some(3), "stderr: {}", common::stderr(&output));
    assert_eq!(
        common::stderr(&output),
        "procly: service web exited with code 7 before it became healthy, its recent output:\ncannot bind\n"
    );
    // Far below the default health timeout of 1 minute.
    assert!(
        start.elapsed() < std::time::Duration::from_secs(10),
        "up took {:?}",
        start.elapsed()
    );
}

#[test]
fn an_http_health_check_waits_for_the_configured_status_on_the_configured_path() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let port = free_port();
    let health = "[services.web.health]\npath = '/status/418'\nstatus = 418\ntimeout = '5s'\n";
    write(&main, "procly.toml", &format!("{}{health}", web_on(port)));

    let output = procly.procly_in(&main, &["up", "web"]);

    assert_success(&output);
    assert_eq!(
        stdout(&output),
        format!("web started http://localhost:{port} port {port}\n")
    );
}

#[test]
fn an_http_health_check_expects_200_by_default_and_never_passes_on_another_status() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let port = free_port();
    let health = "[services.web.health]\npath = '/status/503'\ntimeout = '1s'\n";
    write(&main, "procly.toml", &format!("{}{health}", web_on(port)));

    let output = procly.procly_in(&main, &["up", "web"]);

    assert_eq!(output.status.code(), Some(124), "stderr: {}", common::stderr(&output));
}

#[test]
fn an_http_health_check_given_a_url_sends_its_path_and_host_to_the_lease() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let port = free_port();
    let health = "[services.web.health]\npath = 'http://app.localhost:3000/host/app.localhost:3000'\ntimeout = '5s'\n";
    write(&main, "procly.toml", &format!("{}{health}", web_on(port)));

    let output = procly.procly_in(&main, &["up", "web"]);

    assert_success(&output);
    assert_eq!(
        stdout(&output),
        format!("web started http://localhost:{port} port {port}\n")
    );
}

#[test]
fn an_http_health_check_given_a_url_of_another_host_never_passes() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let port = free_port();
    let health = "[services.web.health]\npath = 'http://other.localhost/host/app.localhost'\ntimeout = '1s'\n";
    write(&main, "procly.toml", &format!("{}{health}", web_on(port)));

    let output = procly.procly_in(&main, &["up", "web"]);

    assert_eq!(output.status.code(), Some(124), "stderr: {}", common::stderr(&output));
}

#[test]
fn a_health_url_that_is_not_http_is_refused_before_anything_starts() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let port = free_port();
    let health = "[services.web.health]\npath = 'https://app.localhost/health'\n";
    write(&main, "procly.toml", &format!("{}{health}", web_on(port)));

    let output = procly.procly_in(&main, &["up", "web"]);

    assert_eq!(output.status.code(), Some(125));
    let stderr = common::stderr(&output);
    assert!(
        stderr.contains("https://app.localhost/health") && stderr.contains("http://"),
        "{stderr}"
    );
}

#[test]
fn down_stops_the_whole_process_group_of_the_service_and_the_daemon_logs_who_asked() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let port = free_port();
    let sleeper = procly.scratch().join("sleeper.pid");
    // The sleep is a second member of the group, it goes only if the whole group is signalled.
    let config = format!(
        "[services.web]\nrun = 'sleep 60 & echo $! > {}; exec {} --port {port}'\nports = {{ http = {port} }}\n",
        sleeper.display(),
        test_server().display()
    );
    write(&main, "procly.toml", &config);
    assert_success(&procly.procly_in(&main, &["up", "web"]));
    let sleeper = common::read_pid(&sleeper);

    let down = procly
        .command()
        .current_dir(&main)
        .args(["down", "web"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("procly down starts");
    let requester = down.id();
    let output = down.wait_with_output().expect("procly down ends");

    assert_success(&output);
    assert_eq!(stdout(&output), "web stopped\n");
    common::wait_until_gone(sleeper);
    assert!(
        TcpStream::connect(("127.0.0.1", port)).is_err(),
        "the server still listens"
    );
    let log = std::fs::read_to_string(procly.state_dir().join("daemon.log")).expect("read the daemon log");
    let line = log
        .lines()
        .find(|line| line.starts_with("down web "))
        .unwrap_or_else(|| panic!("no down line in the daemon log:\n{log}"));
    assert!(
        line.contains(&format!("requested by pid {requester} "))
            && line.contains(&format!("parent pid {} ", std::process::id())),
        "{line}"
    );
}

#[test]
fn down_without_a_service_stops_every_service_of_the_worktree_only() {
    let procly = Procly::new();
    procly.narrow_range();
    let main = git_repository(procly.scratch(), "app");
    let other = common::git_worktree_add(&main, "app-other");
    let (web, api) = (free_port(), free_port());
    let config = format!(
        "[services.web]\nrun = '{server} --port {{ports.http}}'\nports = {{ http = {web} }}\n\
         [services.api]\nrun = '{server} --port {{ports.http}}'\nports = {{ http = {api} }}\n",
        server = test_server().display()
    );
    write(&main, "procly.toml", &config);
    write(&other, "procly.toml", &config);
    for service in ["web", "api"] {
        assert_success(&procly.procly_in(&main, &["up", service]));
    }
    let output = procly.procly_in(&other, &["up", "web"]);
    assert_success(&output);
    let elsewhere = stdout(&output);
    let elsewhere = elsewhere.trim_end().rsplit(' ').next().expect("a port");

    let output = procly.procly_in(&main, &["down"]);

    assert_success(&output);
    assert_eq!(stdout(&output), "api stopped\nweb stopped\n");
    assert!(TcpStream::connect(("127.0.0.1", web)).is_err());
    assert!(TcpStream::connect(("127.0.0.1", api)).is_err());
    let output = procly.procly_in(&other, &["up", "web"]);
    assert_eq!(
        stdout(&output),
        format!("web reused http://localhost:{elsewhere} port {elsewhere}\n")
    );

    let output = procly.procly_in(&main, &["down"]);
    assert_success(&output);
    assert_eq!(stdout(&output), "no services running\n");
}

#[test]
fn down_of_a_service_that_is_not_running_says_so_and_of_an_unknown_one_fails_with_125() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    write(&main, "procly.toml", &web_on(free_port()));

    let output = procly.procly_in(&main, &["down", "web"]);
    assert_success(&output);
    assert_eq!(stdout(&output), "web not running\n");

    let output = procly.procly_in(&main, &["down", "nope"]);
    assert_eq!(output.status.code(), Some(125));
    assert!(
        common::stderr(&output).contains("no service nope is declared"),
        "{}",
        common::stderr(&output)
    );
}

#[test]
fn up_fails_with_125_for_an_undeclared_service_an_invalid_config_or_outside_a_repository() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    write(&main, "procly.toml", &web_on(free_port()));

    let output = procly.procly_in(&main, &["up", "api"]);
    assert_eq!(output.status.code(), Some(125));
    assert!(
        common::stderr(&output).contains("no service api is declared in"),
        "{}",
        common::stderr(&output)
    );

    write(&main, "procly.local.toml", "[services.web]\nrn = 'typo'\n");
    let output = procly.procly_in(&main, &["up", "web"]);
    assert_eq!(output.status.code(), Some(125));
    assert!(
        common::stderr(&output).contains("unknown field `rn`"),
        "{}",
        common::stderr(&output)
    );

    let output = procly.procly(&["up", "web"]);
    assert_eq!(output.status.code(), Some(125));
    assert!(
        common::stderr(&output).contains("is not in a git repository"),
        "{}",
        common::stderr(&output)
    );
}

#[test]
fn up_during_a_down_waits_for_the_stop_and_starts_the_service_again() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let port = free_port();
    let starts = procly.scratch().join("starts");
    // Ignoring SIGTERM, inherited by the exec'd server, keeps the stop going until the SIGKILL 5 seconds later.
    let config = format!(
        "[services.web]\nrun = 'echo start >> {}; trap \"\" TERM; exec {} --port {port}'\nports = {{ http = {port} }}\n",
        starts.display(),
        test_server().display()
    );
    write(&main, "procly.toml", &config);
    assert_success(&procly.procly_in(&main, &["up", "web"]));

    let down = procly
        .command()
        .current_dir(&main)
        .args(["down", "web"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("procly down starts");
    let log = procly.state_dir().join("daemon.log");
    common::eventually("the daemon to log the down", || {
        std::fs::read_to_string(&log).is_ok_and(|log| log.contains("down web "))
    });
    let output = procly.procly_in(&main, &["up", "web"]);

    assert_success(&output);
    assert_eq!(
        stdout(&output),
        format!("web started http://localhost:{port} port {port}\n")
    );
    assert_eq!(start_count(&starts), 2);
    assert_success(&down.wait_with_output().expect("procly down ends"));
}

#[test]
fn up_exits_3_when_the_service_is_killed_by_a_signal_or_stopped_before_it_is_healthy() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let starts = procly.scratch().join("starts");
    let config = format!(
        "[services.killed]\nrun = 'kill -TERM $$'\nports = {{ http = {} }}\n\
         [services.web]\nrun = 'echo $$ >> {}; exec sleep 60'\nports = {{ http = {} }}\n",
        free_port(),
        starts.display(),
        free_port()
    );
    write(&main, "procly.toml", &config);

    let output = procly.procly_in(&main, &["up", "killed"]);
    assert_eq!(output.status.code(), Some(3), "stderr: {}", common::stderr(&output));
    assert_eq!(
        common::stderr(&output),
        "procly: service killed was killed by signal 15 before it became healthy, it printed nothing\n"
    );

    let up = procly
        .command()
        .current_dir(&main)
        .args(["up", "web"])
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("procly up starts");
    common::read_pid(&starts);
    assert_success(&procly.procly_in(&main, &["down", "web"]));
    let output = up.wait_with_output().expect("procly up ends");
    assert_eq!(output.status.code(), Some(3), "stderr: {}", common::stderr(&output));
    assert!(
        common::stderr(&output).contains("web was stopped by procly down before it became healthy"),
        "{}",
        common::stderr(&output)
    );
}

#[test]
fn logs_prints_the_recent_output_of_a_service_of_the_current_worktree() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let port = free_port();
    write(&main, "procly.toml", &web_on(port));
    assert_success(&procly.procly_in(&main, &["up", "web"]));

    common::eventually("the output of web", || {
        stdout(&procly.procly_in(&main, &["logs", "web"])) == format!("listening on {port}\n")
    });
    let unknown = procly.procly_in(&main, &["logs", "api"]);
    assert_eq!(unknown.status.code(), Some(125));
    assert!(
        common::stderr(&unknown).contains("no job or service api"),
        "{}",
        common::stderr(&unknown)
    );
}

/// The size of every output file the Daemon holds in the state directory.
fn output_files(procly: &Procly) -> Vec<u64> {
    std::fs::read_dir(procly.state_dir().join("output")).map_or_else(
        |_| Vec::new(),
        |entries| {
            entries
                .map(|entry| {
                    entry
                        .expect("read the output directory")
                        .metadata()
                        .expect("stat")
                        .len()
                })
                .collect()
        },
    )
}

#[test]
fn the_output_file_of_a_flooding_service_stays_bounded_and_is_removed_once_it_is_stopped() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let port = free_port();
    // 256 KiB printed every few milliseconds, tens of megabytes a second, far past the limit within the second watched.
    let chunk = procly.scratch().join("chunk");
    std::fs::write(&chunk, "procly-flood\n".repeat(256 * 1024 / 13)).expect("write the chunk");
    let run = format!(
        "{} --port {port} & while true; do cat {}; sleep 0.005; done",
        test_server().display(),
        chunk.display()
    );
    write(
        &main,
        "procly.toml",
        &format!("[services.web]\nrun = '{run}'\nports = {{ http = {port} }}\n"),
    );
    assert_success(&procly.procly_in(&main, &["up", "web"]));

    let start = std::time::Instant::now();
    while start.elapsed() < std::time::Duration::from_secs(1) {
        // The output file, and the previous one keeping the last buffer read before the Daemon emptied it.
        let sizes = output_files(&procly);
        assert!(
            (1..=2).contains(&sizes.len()),
            "{} output files for one Service",
            sizes.len()
        );
        assert!(
            sizes.iter().all(|size| *size < 8 * 1024 * 1024),
            "the output files grew to {sizes:?} bytes"
        );
    }
    // The Service is still writing, so the last line may be cut where its write had got to. Every line before it is
    // whole: the buffer dropped its oldest lines, and emptying the file lost nothing in between.
    let logs = stdout(&procly.procly_in(&main, &["logs", "web"]));
    let (lines, last) = logs.rsplit_once('\n').expect("the logs hold whole lines");
    let cut: Vec<(usize, &str)> = lines
        .lines()
        .enumerate()
        .filter(|(_, line)| *line != "procly-flood")
        .take(3)
        .collect();
    assert!(cut.is_empty(), "lines cut in the logs, by their index: {cut:?}");
    assert!("procly-flood".starts_with(last), "the logs end with {last:?}");

    assert_success(&procly.procly_in(&main, &["down", "web"]));
    assert_eq!(output_files(&procly), Vec::<u64>::new());
}
