//! A Service is the tree of its leader, not its process group: a task that leaves the group, as every task of
//! `turbo run dev` does, is still the Service's to watch and to stop.

mod common;

use std::net::TcpStream;
use std::path::Path;

use common::{Procly, assert_success, free_port, git_repository, read_pid, test_server, wait_until_gone};
use nix::unistd::getpgid;

/// Writes `content` to `name` in `dir`.
fn write(dir: &Path, name: &str, content: &str) {
    std::fs::write(dir.join(name), content).expect("write the file");
}

/// A task that cleans up for 100 ms when it is asked to stop, then says so, and that ends by itself after 20 s so
/// that a test that fails never leaves it running.
fn write_cleaning_task(dir: &Path) {
    write(
        dir,
        "task.sh",
        &format!(
            "echo $$ > {pid}\n\
             trap 'sleep 0.1; echo cleaned > {cleaned}; exit 0' TERM\n\
             n=0\n\
             while [ $n -lt 400 ]; do sleep 0.05; n=$((n + 1)); done\n",
            pid = dir.join("task.pid").display(),
            cleaned = dir.join("cleaned").display(),
        ),
    );
}

#[test]
fn a_service_whose_listening_task_moves_to_a_group_of_its_own_turns_healthy_and_down_stops_the_task() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let port = free_port();
    let pid_file = procly.scratch().join("task.pid");
    // The leader forwards `SIGTERM` to its task, as turbo does, and ends with it. The task leads a group of its own.
    let config = format!(
        "[services.web]\n\
         run = '''perl -e 'setpgrp(0,0); exec @ARGV' {server} --port {port} & task=$!; echo $task > {pid}; \
         trap 'kill $task' TERM; wait $task'''\n\
         ports = {{ http = {port} }}\n\
         health = {{ timeout = '5s' }}\n",
        server = test_server().display(),
        pid = pid_file.display(),
    );
    write(&main, "procly.toml", &config);

    // Today the listener is outside the group `lsof` is asked about, so this ends with exit code 124.
    assert_success(&procly.procly_in(&main, &["up", "web"]));
    let task = read_pid(&pid_file);
    assert_eq!(
        getpgid(Some(task)),
        Ok(task),
        "the task leads a process group of its own"
    );

    assert_success(&procly.procly_in(&main, &["down", "web"]));

    wait_until_gone(task);
    assert!(
        TcpStream::connect(("127.0.0.1", port)).is_err(),
        "the task still listens"
    );
}

#[test]
fn down_waits_for_the_whole_tree_before_sending_sigkill_so_that_a_task_in_a_group_of_its_own_cleans_up() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let dir = procly.scratch();
    write_cleaning_task(dir);
    // The leader ends at once on `SIGTERM`. The forwarder is a member of its group that outlives it by 20 ms, then
    // passes `SIGTERM` on to the task, which has a group of its own: turbo is the forwarder, `bun run dev` the leader.
    write(
        dir,
        "forwarder.sh",
        &format!(
            "perl -e 'setpgrp(0,0); exec @ARGV' sh {task} &\n\
             task=$!\n\
             trap 'sleep 0.02; kill -TERM $task; wait $task; exit 0' TERM\n\
             wait\n",
            task = dir.join("task.sh").display(),
        ),
    );
    let config = format!(
        "[services.web]\nrun = 'sh {} & exec sleep 600'\n",
        dir.join("forwarder.sh").display()
    );
    write(&main, "procly.toml", &config);
    assert_success(&procly.procly_in(&main, &["up", "web"]));
    let task = read_pid(&dir.join("task.pid"));

    assert_success(&procly.procly_in(&main, &["down", "web"]));

    // Once `down` answers, the tree is gone: the cleanup of the task has run, so nothing killed it before it ended.
    assert!(
        dir.join("cleaned").exists(),
        "the task was killed before it finished cleaning up"
    );
    wait_until_gone(task);
}

#[test]
fn a_service_whose_leader_ends_on_its_own_leaves_no_task_of_its_tree_behind() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let dir = procly.scratch();
    write_cleaning_task(dir);
    // The leader starts the task in a group of its own, lives long enough for the Daemon to see the task, then ends.
    let config = format!(
        "[services.web]\nrun = '''perl -e 'setpgrp(0,0); exec @ARGV' sh {} & sleep 2'''\n",
        dir.join("task.sh").display()
    );
    write(&main, "procly.toml", &config);
    assert_success(&procly.procly_in(&main, &["up", "web"]));
    let task = read_pid(&dir.join("task.pid"));

    // Today the group of the leader is killed and the task, which is in no such group, keeps running.
    wait_until_gone(task);
}

#[test]
fn a_task_started_late_whose_parent_ends_at_once_is_still_stopped_with_the_service() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let dir = procly.scratch();
    write_cleaning_task(dir);
    // The task starts 0.2 s in, in a group of its own, and its parent is gone 0.3 s in, long before any periodic look
    // at the processes of the Service could have seen the task.
    let config = format!(
        "[services.web]\nrun = '''sleep 0.2; perl -e 'setpgrp(0,0); exec @ARGV' sh {} & sleep 0.3'''\n",
        dir.join("task.sh").display()
    );
    write(&main, "procly.toml", &config);
    assert_success(&procly.procly_in(&main, &["up", "web"]));
    let task = read_pid(&dir.join("task.pid"));

    // The leader ended on its own, nothing of its tree may outlive it.
    wait_until_gone(task);
}

#[test]
fn down_asks_the_tasks_a_leader_does_not_forward_to_for_a_clean_stop_without_waiting_for_the_stop_timeout() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let dir = procly.scratch();
    write_cleaning_task(dir);
    // The leader ends on `SIGTERM` and forwards it to nobody: the task, which has a group of its own, only hears of the
    // stop from the Daemon.
    let config = format!(
        "[services.web]\nrun = '''perl -e 'setpgrp(0,0); exec @ARGV' sh {} & exec sleep 600'''\n",
        dir.join("task.sh").display()
    );
    write(&main, "procly.toml", &config);
    assert_success(&procly.procly_in(&main, &["up", "web"]));
    let task = read_pid(&dir.join("task.pid"));

    let started = std::time::Instant::now();
    assert_success(&procly.procly_in(&main, &["down", "web"]));

    assert!(
        dir.join("cleaned").exists(),
        "the task was killed without being asked to stop"
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(3),
        "down waited for the stop timeout: {:?}",
        started.elapsed()
    );
    wait_until_gone(task);
}

#[test]
fn a_process_that_leaves_the_session_of_the_service_is_not_part_of_it_and_survives_down() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let dir = procly.scratch();
    write_cleaning_task(dir);
    // A shared daemon that a Service starts, such as a build server, detaches into a session of its own.
    let config = format!(
        "[services.web]\nrun = '''perl -e 'use POSIX; POSIX::setsid(); exec @ARGV' sh {} & exec sleep 600'''\n",
        dir.join("task.sh").display()
    );
    write(&main, "procly.toml", &config);
    assert_success(&procly.procly_in(&main, &["up", "web"]));
    let task = read_pid(&dir.join("task.pid"));

    assert_success(&procly.procly_in(&main, &["down", "web"]));

    assert!(
        common::is_alive(task),
        "a process of another session was stopped with the service"
    );
    // The test stops it itself.
    nix::sys::signal::kill(task, nix::sys::signal::Signal::SIGTERM).expect("stop the detached process");
    wait_until_gone(task);
}
