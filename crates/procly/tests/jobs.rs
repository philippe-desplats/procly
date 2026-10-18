mod common;

use std::time::{Duration, Instant};

use common::{Procly, assert_success, stdout};

#[test]
fn wait_prints_the_output_and_exits_with_the_job_exit_code() {
    let procly = Procly::new();

    let job = procly.run(&["echo hello; echo world; exit 3"]);
    let output = procly.procly(&["wait", &job]);

    assert_eq!(output.status.code(), Some(3), "stderr: {}", common::stderr(&output));
    assert_eq!(stdout(&output), "hello\nworld\n");
}

#[test]
fn wait_exits_zero_for_a_successful_job() {
    let procly = Procly::new();

    let job = procly.run(&["true"]);
    let output = procly.procly(&["wait", &job]);

    assert_success(&output);
    assert_eq!(stdout(&output), "");
}

#[test]
fn run_returns_the_job_identifier_without_waiting_for_the_command() {
    let procly = Procly::new();

    let start = Instant::now();
    let job = procly.run(&["sleep 30"]);

    assert!(
        start.elapsed() < Duration::from_secs(5),
        "run took {:?}",
        start.elapsed()
    );
    let logs = procly.procly(&["logs", &job]);
    assert_success(&logs);
}

#[test]
fn logs_shows_the_output_of_a_running_job() {
    let procly = Procly::new();
    let release = procly.scratch().join("release");

    let job = procly.run(&[&format!(
        "echo started; while [ ! -e {} ]; do sleep 0.05; done; echo released",
        release.display()
    )]);

    common::eventually("the job to print its first line", || {
        stdout(&procly.procly(&["logs", &job])) == "started\n"
    });
    std::fs::write(&release, "").unwrap();
    assert_success(&procly.procly(&["wait", &job]));
    assert_eq!(stdout(&procly.procly(&["logs", &job])), "started\nreleased\n");
}

#[test]
fn a_job_past_its_timeout_has_its_whole_process_tree_killed_and_wait_exits_124() {
    let procly = Procly::new();
    let pid_file = procly.scratch().join("grandchild.pid");

    // The shell leads the Job, the backgrounded subshell and its sleep are descendants in the same tree. They
    // ignore the hangup sent when the leader dies and the polite SIGTERM, so only a SIGKILL to the group stops them.
    let job = procly.run(&[
        "--timeout",
        "1s",
        &format!(
            "(trap '' HUP TERM; sleep 60; echo late) & echo $! > {}; echo waiting; wait",
            pid_file.display()
        ),
    ]);
    let grandchild = common::read_pid(&pid_file);
    let output = procly.procly(&["wait", &job]);

    assert_eq!(output.status.code(), Some(124), "stderr: {}", common::stderr(&output));
    assert_eq!(stdout(&output), "waiting\n");
    assert!(
        common::stderr(&output).contains("timed out after 1s"),
        "stderr: {}",
        common::stderr(&output)
    );
    common::wait_until_gone(grandchild);
}

#[test]
fn a_job_ends_with_its_leader_and_takes_the_processes_it_left_behind() {
    let procly = Procly::new();
    let pid_file = procly.scratch().join("leftover.pid");

    // The leftover ignores the hangup sent when the leader exits, and would keep the terminal open forever. The
    // leader waits for its pid, so the leftover surely exists when the leader ends.
    let start = Instant::now();
    let job = procly.run(&[&format!(
        "(trap '' HUP; sleep 30 & echo $! > {pid}; wait) & while [ ! -s {pid} ]; do sleep 0.01; done; echo done",
        pid = pid_file.display()
    )]);
    let leftover = common::read_pid(&pid_file);
    let output = procly.procly(&["wait", &job]);

    assert_success(&output);
    assert_eq!(stdout(&output), "done\n");
    common::wait_until_gone(leftover);
    assert!(
        start.elapsed() < Duration::from_secs(10),
        "wait took {:?}",
        start.elapsed()
    );
}

#[test]
fn wait_reports_a_job_killed_by_a_signal_with_the_shell_convention() {
    let procly = Procly::new();

    let job = procly.run(&["kill -TERM $$"]);
    let output = procly.procly(&["wait", &job]);

    assert_eq!(output.status.code(), Some(128 + 15));
}

#[test]
fn the_job_runs_in_the_callers_directory_and_environment() {
    let procly = Procly::new();

    let output = procly
        .command()
        .args(["run", "pwd; echo $PROCLY_TEST_VALUE"])
        .env("PROCLY_TEST_VALUE", "from-caller")
        .output()
        .unwrap();
    assert_success(&output);
    let job = stdout(&output).trim().to_owned();
    let output = procly.procly(&["wait", &job]);

    let cwd = procly.scratch().canonicalize().unwrap();
    assert_eq!(stdout(&output), format!("{}\nfrom-caller\n", cwd.display()));
}

#[test]
fn wait_and_logs_fail_with_125_for_an_unknown_job() {
    let procly = Procly::new();

    for verb in ["wait", "logs"] {
        let output = procly.procly(&[verb, "nosuchjob"]);
        assert_eq!(output.status.code(), Some(125), "{verb}");
        assert!(
            common::stderr(&output).contains("no job nosuchjob"),
            "{verb}: {}",
            common::stderr(&output)
        );
    }
}
