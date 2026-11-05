mod common;

use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::process::Stdio;

use common::{Procly, assert_success, stderr, stdout};

#[test]
fn the_first_command_starts_the_daemon_on_a_socket_only_the_user_can_reach() {
    let procly = Procly::new();
    let socket = procly.state_dir().join("procly.sock");
    assert!(!socket.exists());

    procly.run(&["true"]);

    let metadata = std::fs::metadata(&socket).expect("the daemon created its socket");
    assert!(metadata.file_type().is_socket());
    assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    let state_dir = std::fs::metadata(procly.state_dir()).unwrap();
    assert_eq!(state_dir.permissions().mode() & 0o777, 0o700);
}

#[test]
fn concurrent_first_commands_share_one_daemon() {
    let procly = Procly::new();

    let runs: Vec<_> = (0..6)
        .map(|i| {
            procly
                .command()
                .args(["run", &format!("echo {i}")])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    let jobs: Vec<_> = runs
        .into_iter()
        .map(|run| {
            let output = run.wait_with_output().unwrap();
            assert_success(&output);
            common::started(&output).0
        })
        .collect();

    // Every Job is known to the one Daemon that answers now, so none went to a Daemon that lost the race.
    for (i, job) in jobs.iter().enumerate() {
        let output = procly.procly(&["wait", job]);
        assert_success(&output);
        assert_eq!(stdout(&output), format!("{i}\n"));
    }
}

#[test]
fn a_stale_socket_left_by_a_crashed_daemon_is_replaced() {
    let procly = Procly::new();
    std::fs::create_dir_all(procly.state_dir()).unwrap();
    let stale = std::os::unix::net::UnixListener::bind(procly.state_dir().join("procly.sock")).unwrap();
    drop(stale);

    let job = procly.run(&["echo alive"]);

    assert_eq!(stdout(&procly.procly(&["wait", &job])), "alive\n");
}

#[test]
fn stopping_the_daemon_kills_the_jobs_it_still_runs() {
    let procly = Procly::new();
    let pid_file = procly.scratch().join("job.pid");
    procly.run(&[&format!("trap '' HUP TERM; echo $$ > {}; sleep 60", pid_file.display())]);
    let job = common::read_pid(&pid_file);

    // Dropping the harness sends SIGTERM to the Daemon and waits for it to exit.
    drop(procly);

    common::wait_until_gone(job);
}

#[test]
fn daemon_stop_with_no_daemon_exits_0_and_says_so() {
    let procly = Procly::new();

    let text = procly.procly(&["daemon", "stop"]);
    let json = procly.procly(&["daemon", "stop", "--json"]);

    assert_success(&text);
    assert_eq!(stdout(&text), "");
    assert_eq!(stderr(&text), "procly: no Daemon running\n");
    assert_eq!(
        common::json_succeeded(&json),
        serde_json::json!({"schema_version": 1, "outcome": "not_running", "pid": null})
    );
    assert!(
        !procly.state_dir().join("procly.sock").exists(),
        "stopping never starts a Daemon"
    );
}

/// Makes the pid file a stale one: it names a process that runs, and no Daemon holds the lock.
#[test]
fn daemon_stop_treats_a_stale_pid_file_as_no_daemon_and_signals_nobody() {
    let procly = Procly::new();
    let mut bystander = std::process::Command::new("sleep").arg("60").spawn().unwrap();
    std::fs::write(procly.state_dir().join("daemon.pid"), format!("{}\n", bystander.id())).unwrap();

    let output = procly.procly(&["daemon", "stop", "--json"]);

    assert_eq!(
        common::json_succeeded(&output),
        serde_json::json!({"schema_version": 1, "outcome": "not_running", "pid": null})
    );
    assert!(
        bystander.try_wait().unwrap().is_none(),
        "the process of the pid file was left alone"
    );
    bystander.kill().unwrap();
    bystander.wait().unwrap();
}

#[test]
fn daemon_stop_stops_the_daemon_with_its_services_and_reports_how_long_it_waited() {
    let procly = Procly::new();
    let main = common::git_repository(procly.scratch(), "app");
    let port = common::free_port();
    std::fs::write(
        main.join("procly.toml"),
        format!(
            "[services.web]\nrun = '{} --port {{ports.http}}'\nports = {{ http = {port} }}\n",
            common::test_server().display()
        ),
    )
    .unwrap();
    assert_success(&procly.procly_in(&main, &["up", "web"]));
    let pid = std::fs::read_to_string(procly.state_dir().join("daemon.pid")).unwrap();
    let pid: i32 = pid.trim().parse().unwrap();

    let output = procly.procly(&["daemon", "stop", "--json"]);

    let document = common::json_succeeded(&output);
    assert_eq!(document["schema_version"], 1);
    assert_eq!(document["outcome"], "stopped");
    assert_eq!(document["pid"], pid);
    assert!(document["waited_ms"].is_u64(), "waited_ms is a number: {document}");
    assert!(!common::is_alive(nix::unistd::Pid::from_raw(pid)));
    assert!(!procly.state_dir().join("procly.sock").exists());
    assert!(
        std::net::TcpStream::connect(("127.0.0.1", port)).is_err(),
        "the Service the Daemon supervised was stopped with it"
    );
    let again = procly.procly(&["daemon", "stop"]);
    assert_success(&again);
    assert_eq!(stderr(&again), "procly: no Daemon running\n");
}

/// A stand-in for a Daemon that ignores `SIGTERM`: it holds the lock of the state directory, listens on the socket and
/// writes the pid of a process that traps the signal. Dropping it ends that process, which the harness would otherwise
/// signal in vain when it drops.
struct DeafDaemon {
    process: std::process::Child,
    _lock: std::fs::File,
    _socket: std::os::unix::net::UnixListener,
    pid_file: std::path::PathBuf,
}

impl DeafDaemon {
    fn start(procly: &Procly) -> Self {
        use std::io::BufRead as _;
        let mut process = std::process::Command::new("sh")
            .args(["-c", "trap '' TERM; echo ready; while :; do sleep 1; done"])
            .stdout(Stdio::piped())
            .spawn()
            .expect("start the process that ignores SIGTERM");
        let mut ready = String::new();
        std::io::BufReader::new(process.stdout.take().expect("its stdout is piped"))
            .read_line(&mut ready)
            .expect("wait until it traps SIGTERM");
        let lock = std::fs::File::create(procly.state_dir().join("daemon.lock")).expect("create the lock file");
        lock.lock().expect("hold the lock");
        let socket = std::os::unix::net::UnixListener::bind(procly.state_dir().join("procly.sock"))
            .expect("listen on the socket");
        let pid_file = procly.state_dir().join("daemon.pid");
        std::fs::write(&pid_file, format!("{}\n", process.id())).expect("write the pid file");
        Self {
            process,
            _lock: lock,
            _socket: socket,
            pid_file,
        }
    }
}

impl Drop for DeafDaemon {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
        let _ = std::fs::remove_file(&self.pid_file);
    }
}

#[test]
fn daemon_stop_gives_up_after_10_seconds_on_a_daemon_that_ignores_sigterm_and_never_forces_it() {
    let procly = Procly::new();
    let mut daemon = DeafDaemon::start(&procly);
    let pid = daemon.process.id();

    let started = std::time::Instant::now();
    let output = procly.procly(&["daemon", "stop", "--json"]);

    assert_eq!(output.status.code(), Some(125), "{}", common::failure_message(&output));
    assert_eq!(stdout(&output), "", "no document when the Daemon is still there");
    assert_eq!(
        stderr(&output),
        format!("procly: the Daemon (pid {pid}) did not stop within 10 s\n")
    );
    assert!(started.elapsed() >= std::time::Duration::from_secs(10));
    assert!(
        daemon.process.try_wait().unwrap().is_none(),
        "the Daemon was not forced"
    );
}
