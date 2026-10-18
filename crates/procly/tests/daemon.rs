mod common;

use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::process::Stdio;

use common::{Procly, assert_success, stdout};

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
