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

/// Runs `start`, which starts a Daemon, while no more than 3 other starts of this binary are running.
///
/// A new Daemon copies its executable, 46 MB, and runs the copy once, which macOS has the system assess for about 0.4 s,
/// one file after another. Measured with `sample` while `up` failed: the test server of a Service stood at its first
/// instruction, in `_dyld_start`, for the whole 5 s of its health timeout, and `up` ended with exit code 124. That is the
/// queue of the 19 Daemons the tests of this file started at once. The tests still run at the same time, only the starts
/// of their Daemons are limited, so that the queue is never longer than 3 files, about 1.2 s.
fn few_daemons_at_a_time<T>(start: impl FnOnce() -> T) -> T {
    static STARTING: std::sync::Mutex<usize> = std::sync::Mutex::new(0);
    static FREED: std::sync::Condvar = std::sync::Condvar::new();
    const AT_A_TIME: usize = 3;
    let mut starting = STARTING.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    while *starting >= AT_A_TIME {
        starting = FREED.wait(starting).unwrap_or_else(std::sync::PoisonError::into_inner);
    }
    *starting += 1;
    drop(starting);
    let started = start();
    *STARTING.lock().unwrap_or_else(std::sync::PoisonError::into_inner) -= 1;
    FREED.notify_one();
    started
}

/// Starts the Daemon of `procly` with a first command that asks for nothing, see [`few_daemons_at_a_time`].
fn start_daemon(procly: &Procly, dir: &Path) {
    few_daemons_at_a_time(|| assert_success(&procly.procly_in(dir, &["status"])));
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
    start_daemon(&procly, &main);
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
    start_daemon(&procly, &main);
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
    start_daemon(&procly, &main);
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
    start_daemon(&procly, &main);
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
    start_daemon(&procly, &main);
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
    start_daemon(&procly, &main);
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

/// Whether `pid` is gone, or ends within `within`.
fn gone_within(pid: nix::unistd::Pid, within: std::time::Duration) -> bool {
    let start = std::time::Instant::now();
    while common::is_alive(pid) {
        if start.elapsed() >= within {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    true
}

/// What `ps` prints as the state of `pid`, empty once the process is gone.
fn process_state(pid: nix::unistd::Pid) -> String {
    let output = std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .expect("ps runs");
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

#[test]
fn the_daemon_keeps_the_ended_leader_of_a_stopping_service_unreaped_until_its_whole_tree_is_gone() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let dir = procly.scratch();
    // The task leads a group of its own and takes 1 s to clean up once it is asked to stop.
    write(
        dir,
        "task.sh",
        &format!(
            "echo $$ > {pid}\n\
             trap 'echo stopping > {stopping}; sleep 1; echo cleaned > {cleaned}; exit 0' TERM\n\
             n=0\n\
             while [ $n -lt 400 ]; do sleep 0.05; n=$((n + 1)); done\n",
            pid = dir.join("task.pid").display(),
            stopping = dir.join("stopping").display(),
            cleaned = dir.join("cleaned").display(),
        ),
    );
    // The leader ends at once on `SIGTERM` and forwards it to nobody.
    let config = format!(
        "[services.web]\nrun = '''echo $$ > {leader}; perl -e 'setpgrp(0,0); exec @ARGV' sh {task} & exec sleep 600'''\n",
        leader = dir.join("leader.pid").display(),
        task = dir.join("task.sh").display(),
    );
    write(&main, "procly.toml", &config);
    start_daemon(&procly, &main);
    assert_success(&procly.procly_in(&main, &["up", "web"]));
    let (leader, task) = (read_pid(&dir.join("leader.pid")), read_pid(&dir.join("task.pid")));

    let down = procly
        .command()
        .current_dir(&main)
        .args(["down", "web"])
        .spawn()
        .expect("the procly binary runs");
    common::eventually("the task to be asked to stop", || dir.join("stopping").exists());

    // The leader has ended, since the Daemon asked the task to stop only then. While the task cleans up, the session of
    // the leader still has a member, and the pid of the leader must not be free for another process to take, and with it
    // the id of the session: the Daemon has not reaped it.
    let state = process_state(leader);
    assert!(
        state.starts_with('Z'),
        "the Daemon reaped the leader while its tree was still stopping, ps shows {state:?}"
    );
    let output = down.wait_with_output().expect("down ends");
    assert_success(&output);
    assert!(dir.join("cleaned").exists(), "the task was killed before it cleaned up");
    wait_until_gone(task);
    common::eventually("the leader to be reaped", || process_state(leader).is_empty());
}

#[test]
fn a_child_started_by_a_task_while_the_service_stops_does_not_survive_down() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let dir = procly.scratch();
    // The task leads a group of its own. Asked to stop, it starts a child in another group of its own that ignores
    // `SIGTERM`, and ends at once, before anything could have listed the child.
    write(
        dir,
        "forker.sh",
        &format!(
            "trap 'perl -e '\"'\"'setpgrp(0,0); $SIG{{TERM}}=q(IGNORE); open my $f, q(>), q({child}); print $f $$; \
             close $f; sleep 30'\"'\"' & exit 0' TERM\n\
             echo $$ > {pid}\n\
             while :; do sleep 0.1; done\n",
            child = dir.join("child.pid").display(),
            pid = dir.join("forker.pid").display(),
        ),
    );
    let config = format!(
        "[services.web]\nrun = '''perl -e 'setpgrp(0,0); exec @ARGV' sh {} & exec sleep 600'''\n",
        dir.join("forker.sh").display()
    );
    write(&main, "procly.toml", &config);
    start_daemon(&procly, &main);
    assert_success(&procly.procly_in(&main, &["up", "web"]));
    let forker = read_pid(&dir.join("forker.pid"));

    assert_success(&procly.procly_in(&main, &["down", "web"]));

    let child = read_pid(&dir.join("child.pid"));
    let survived = !gone_within(child, std::time::Duration::from_secs(2));
    // Whatever the outcome, the test leaves nothing behind.
    let _ = nix::sys::signal::kill(child, nix::sys::signal::Signal::SIGKILL);
    wait_until_gone(forker);
    assert!(
        !survived,
        "a child the task started while the service stopped survived down: {}",
        procly.daemon_log()
    );
    assert!(
        !procly.daemon_log().contains("taken for another tree's"),
        "the Daemon took a process of the service for another tree's: {}",
        procly.daemon_log()
    );
}

/// The cases where the Daemon cannot list the processes of the system. The Daemon is told to refuse the listing through
/// `PROCLY_TEST_LISTING`, on every platform, while it still reads the identity of one process, as it does when the
/// system refuses a full listing but answers for a pid.
mod without_the_process_list {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use nix::sys::signal::{Signal, killpg};
    use nix::unistd::Pid;

    use super::{
        Path, Procly, assert_success, common, free_port, git_repository, read_pid, test_server, wait_until_gone, write,
    };

    /// The directory the Daemon logs its listings in and is refused them through.
    struct Listing {
        dir: PathBuf,
    }

    impl Listing {
        fn new(scratch: &Path) -> Self {
            let dir = scratch.join("listing");
            std::fs::create_dir(&dir).expect("create the listing directory");
            Self { dir }
        }

        fn refuse(&self) {
            std::fs::write(self.dir.join("refuse"), "").expect("write the flag");
        }

        /// How many times the Daemon listed the processes since the log was last emptied.
        fn count(&self) -> usize {
            std::fs::read_to_string(self.dir.join("calls"))
                .unwrap_or_default()
                .lines()
                .count()
        }

        /// Lets the Daemon list the processes again.
        fn restore(&self) {
            std::fs::remove_file(self.dir.join("refuse")).expect("remove the flag");
        }

        /// Starts the Daemon with the hook set, see [`super::few_daemons_at_a_time`].
        fn start_daemon(&self, procly: &Procly, dir: &Path) {
            super::few_daemons_at_a_time(|| assert_success(&self.procly_in(procly, dir, &["status"])));
        }

        fn empty_the_log(&self) {
            std::fs::write(self.dir.join("calls"), "").expect("empty the log");
        }

        /// Runs `procly` from `dir` with the hook set, which the Daemon it starts keeps.
        fn procly_in(&self, procly: &Procly, dir: &Path, args: &[&str]) -> std::process::Output {
            procly
                .command()
                .current_dir(dir)
                .env("PROCLY_TEST_LISTING", &self.dir)
                .args(args)
                .output()
                .expect("the procly binary runs")
        }
    }

    /// Kills the process group led by `0` when the test ends, passed or failed.
    struct ReapGroup(Pid);

    impl Drop for ReapGroup {
        fn drop(&mut self) {
            let _ = killpg(self.0, Signal::SIGKILL);
        }
    }

    /// A process that has nothing to do with the Service and listens on `port`, ended when dropped.
    struct Stranger(std::process::Child);

    impl Stranger {
        fn listen(port: u16) -> Self {
            let child = std::process::Command::new(test_server())
                .args(["--port", &port.to_string()])
                .spawn()
                .expect("the test server starts");
            let stranger = Self(child);
            common::eventually("the stranger to listen", || {
                std::net::TcpStream::connect(("127.0.0.1", port)).is_ok()
            });
            stranger
        }

        fn pid(&self) -> Pid {
            Pid::from_raw(i32::try_from(self.0.id()).expect("a pid fits in an i32"))
        }
    }

    impl Drop for Stranger {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// A Service of the form of `turbo run dev` that is up: its leader only sleeps and its listener is a task in a group
    /// of its own. Both are reaped when the test ends.
    struct Kept {
        main: PathBuf,
        port: u16,
        starts: PathBuf,
        leader: Pid,
        listener: Pid,
        _reap: [ReapGroup; 2],
    }

    /// Where the listener of a [`Kept`] Service runs.
    #[derive(Debug, Clone, Copy)]
    enum Listener {
        /// In a process group of its own, as a task of turbo is.
        OwnGroup,
        /// In the process group of the leader, as the children of `npm run dev` are.
        LeaderGroup,
    }

    impl Kept {
        fn up(procly: &Procly, listing: &Listing) -> Self {
            Self::up_with(procly, listing, Listener::OwnGroup)
        }

        fn up_with(procly: &Procly, listing: &Listing, form: Listener) -> Self {
            let main = git_repository(procly.scratch(), "app");
            let port = free_port();
            let dir = procly.scratch();
            let (starts, leader, listener) = (dir.join("starts"), dir.join("leader"), dir.join("pid"));
            let own_group = match form {
                Listener::OwnGroup => "perl -e 'setpgrp(0,0); exec @ARGV' ",
                Listener::LeaderGroup => "",
            };
            let config = format!(
                "[services.web]\nrun = '''echo start >> {starts}; echo $$ > {leader}; \
                 {own_group}{server} --port {port} & echo $! > {pid}; exec sleep 600'''\n\
                 ports = {{ http = {port} }}\nhealth = {{ timeout = '5s' }}\n",
                starts = starts.display(),
                leader = leader.display(),
                pid = listener.display(),
                server = test_server().display()
            );
            write(&main, "procly.toml", &config);
            listing.start_daemon(procly, &main);
            assert_success(&listing.procly_in(procly, &main, &["up", "web"]));
            let (leader, listener) = (read_pid(&leader), read_pid(&listener));
            Self {
                main,
                port,
                starts,
                leader,
                listener,
                _reap: [ReapGroup(leader), ReapGroup(listener)],
            }
        }

        fn starts(&self) -> usize {
            std::fs::read_to_string(&self.starts)
                .unwrap_or_default()
                .lines()
                .count()
        }

        /// Shortens the health timeout of the Service, for a test whose next `up` cannot turn healthy and would
        /// otherwise read the processes of the system for the whole 5 s, while the other tests are starting theirs.
        fn bound_the_health_wait(&self) {
            let path = self.main.join("procly.toml");
            let config = std::fs::read_to_string(&path).expect("read the configuration");
            write(
                &self.main,
                "procly.toml",
                &config.replace("timeout = '5s'", "timeout = '2s'"),
            );
        }
    }

    #[test]
    fn a_service_found_running_while_the_process_list_cannot_be_read_stays_attached_and_down_stops_it() {
        let procly = Procly::new();
        let main = git_repository(procly.scratch(), "app");
        let port = free_port();
        let (starts, pid) = (procly.scratch().join("starts"), procly.scratch().join("pid"));
        let config = format!(
            "[services.web]\nrun = 'echo start >> {starts}; echo $$ > {pid}; exec {server}'\nports = {{ http = {port} }}\n",
            starts = starts.display(),
            pid = pid.display(),
            server = test_server().display()
        );
        write(&main, "procly.toml", &config);
        let listing = Listing::new(procly.scratch());
        listing.start_daemon(&procly, &main);
        assert_success(&listing.procly_in(&procly, &main, &["up", "web"]));
        let server = read_pid(&pid);
        let _reap = ReapGroup(server);
        procly.kill_daemon();

        // The next Daemon cannot list the processes, though it reads the start of the leader it finds.
        listing.refuse();
        listing.start_daemon(&procly, &main);
        let output = listing.procly_in(&procly, &main, &["up", "web"]);

        assert_success(&output);
        common::assert_declared_port(
            port,
            &common::stdout(&output),
            &format!("web reused http://localhost:{port} port {port}\n"),
        );
        assert_eq!(
            std::fs::read_to_string(&starts).unwrap_or_default().lines().count(),
            1,
            "the service was started a second time"
        );
        assert!(
            !procly.daemon_log().contains("was stopped"),
            "the Daemon dropped a service that runs: {}",
            procly.daemon_log()
        );

        let stopping = Instant::now();
        assert_success(&listing.procly_in(&procly, &main, &["down", "web"]));
        wait_until_gone(server);
        assert!(
            stopping.elapsed() < Duration::from_secs(3),
            "down waited for the stop timeout: {:?}",
            stopping.elapsed()
        );
    }

    #[test]
    fn a_service_whose_listener_runs_in_a_group_of_its_own_is_kept_and_left_alone_when_the_process_list_cannot_be_read()
    {
        let procly = Procly::new();
        let main = git_repository(procly.scratch(), "app");
        let port = free_port();
        let (starts, pid) = (procly.scratch().join("starts"), procly.scratch().join("pid"));
        // The leader only sleeps, the listener is a task in a group of its own: the form of `turbo run dev`.
        let config = format!(
            "[services.web]\nrun = '''echo start >> {starts}; perl -e 'setpgrp(0,0); exec @ARGV' {server} --port {port} & \
             echo $! > {pid}; exec sleep 600'''\nports = {{ http = {port} }}\nhealth = {{ timeout = '5s' }}\n",
            starts = starts.display(),
            pid = pid.display(),
            server = test_server().display()
        );
        write(&main, "procly.toml", &config);
        let listing = Listing::new(procly.scratch());
        listing.start_daemon(&procly, &main);
        assert_success(&listing.procly_in(&procly, &main, &["up", "web"]));
        let listener = read_pid(&pid);
        let _reap = ReapGroup(listener);
        procly.kill_daemon();

        // Nothing listens on the group of the leader, and the Daemon cannot list the processes to find who does.
        listing.refuse();
        listing.start_daemon(&procly, &main);
        let output = listing.procly_in(&procly, &main, &["up", "web"]);

        assert_success(&output);
        common::assert_declared_port(
            port,
            &common::stdout(&output),
            &format!("web reused http://localhost:{port} port {port}\n"),
        );
        assert_eq!(
            std::fs::read_to_string(&starts).unwrap_or_default().lines().count(),
            1,
            "the service was started a second time"
        );
        let log = procly.daemon_log();
        assert!(
            !log.contains("was stopped"),
            "the Daemon stopped a service that runs: {log}"
        );
        assert!(
            log.contains("could not list the processes"),
            "the Daemon did not say why it kept the service: {log}"
        );
        assert!(common::is_alive(listener), "the listener was stopped");
        // It cannot say that what answers is the service, and says so rather than claiming it is healthy.
        assert!(
            common::stderr(&output).contains("cannot tell that web is what listens on port"),
            "up did not say it could not tell: {}",
            common::stderr(&output)
        );
    }

    #[test]
    fn down_while_the_process_list_cannot_be_read_stops_the_leader_without_waiting_for_the_stop_timeout() {
        let procly = Procly::new();
        let main = git_repository(procly.scratch(), "app");
        let dir = procly.scratch();
        super::write_cleaning_task(dir);
        let config = format!(
            "[services.web]\nrun = '''perl -e 'setpgrp(0,0); exec @ARGV' sh {} & exec sleep 600'''\n",
            dir.join("task.sh").display()
        );
        write(&main, "procly.toml", &config);
        let listing = Listing::new(dir);
        listing.start_daemon(&procly, &main);
        assert_success(&listing.procly_in(&procly, &main, &["up", "web"]));
        let task = read_pid(&dir.join("task.pid"));
        // The task is in a group the Daemon cannot find without the listing, it is left to its own devices.
        let _reap = ReapGroup(task);

        listing.refuse();
        let stopping = Instant::now();
        assert_success(&listing.procly_in(&procly, &main, &["down", "web"]));

        assert!(
            stopping.elapsed() < Duration::from_secs(3),
            "down waited for the stop timeout: {:?}",
            stopping.elapsed()
        );
        assert!(
            !procly
                .daemon_log()
                .contains("only the process group of the leader is killed"),
            "the Daemon said it killed a group that was already empty: {}",
            procly.daemon_log()
        );
    }

    #[test]
    fn a_stop_that_runs_to_the_stop_timeout_does_not_read_the_process_list_every_20_ms() {
        let procly = Procly::new();
        let main = git_repository(procly.scratch(), "app");
        let dir = procly.scratch();
        // A task in a group of its own that ignores `SIGTERM`, so that the stop lasts until the stop timeout.
        write(
            dir,
            "stubborn.sh",
            &format!(
                "echo $$ > {pid}\n\
                 trap '' TERM\n\
                 n=0\n\
                 while [ $n -lt 400 ]; do sleep 0.05; n=$((n + 1)); done\n",
                pid = dir.join("task.pid").display(),
            ),
        );
        let config = format!(
            "[services.web]\nrun = '''perl -e 'setpgrp(0,0); exec @ARGV' sh {} & exec sleep 600'''\n",
            dir.join("stubborn.sh").display()
        );
        write(&main, "procly.toml", &config);
        let listing = Listing::new(dir);
        listing.start_daemon(&procly, &main);
        assert_success(&listing.procly_in(&procly, &main, &["up", "web"]));
        let task = read_pid(&dir.join("task.pid"));

        listing.empty_the_log();
        assert_success(&listing.procly_in(&procly, &main, &["down", "web"]));

        wait_until_gone(task);
        // 5 s at 1 listing every 20 ms were 250 listings, 1 in 20 s was never needed to tell the tree was not gone.
        let listings = listing.count();
        assert!(listings <= 30, "the stop read the process list {listings} times");
    }

    /// The Daemon that found the Service of `kept` cannot list the processes and keeps it as it was, because something
    /// answers on its Lease. Whatever answers is `stranger` when it is given, which took the port once the listener ended.
    fn restart_unable_to_list(procly: &Procly, listing: &Listing, kept: &Kept, stranger: bool) -> Option<Stranger> {
        procly.kill_daemon();
        let held = stranger.then(|| {
            // The listener ends while no Daemon runs, a process that has nothing to do with the Service takes its port.
            killpg(kept.listener, Signal::SIGKILL).expect("end the listener");
            // Its parent, the sleeping leader, never reaps it: it is a zombie that holds nothing.
            common::eventually("the port of the listener to be free", || {
                std::net::TcpStream::connect(("127.0.0.1", kept.port)).is_err()
            });
            Stranger::listen(kept.port)
        });
        listing.refuse();
        listing.start_daemon(procly, &kept.main);
        assert!(
            procly.daemon_log().contains("it kept the Service as it was"),
            "the Daemon did not keep the service as it was: {}",
            procly.daemon_log()
        );
        held
    }

    fn a_stranger_holds_the_port_of_a_kept_service(wait_for_the_judgement: bool) {
        let procly = Procly::new();
        let listing = Listing::new(procly.scratch());
        let kept = Kept::up(&procly, &listing);
        let stranger = restart_unable_to_list(&procly, &listing, &kept, true).expect("a stranger");

        listing.restore();
        if wait_for_the_judgement {
            // Nobody asks for the service, the Daemon judges it by itself as soon as it can list the processes.
            common::eventually("the Daemon to judge the kept service", || {
                procly.daemon_log().contains("no longer listened on its Lease")
            });
        }
        // What `up` starts instead cannot bind the port and never turns healthy: bound the wait, and the readings of
        // the processes it makes meanwhile.
        kept.bound_the_health_wait();
        listing.empty_the_log();
        let output = listing.procly_in(&procly, &kept.main, &["up", "web"]);

        let listings = listing.count();
        assert!(
            listings < 20,
            "up read the processes {listings} times while a stranger held the port for 2 s"
        );
        assert!(
            !common::stdout(&output).contains("reused"),
            "up answered that the service of another process was reused: {}",
            common::failure_message(&output)
        );
        assert!(
            !output.status.success(),
            "up found a service healthy on a port a stranger holds: {}",
            common::failure_message(&output)
        );
        assert!(
            common::is_alive(stranger.pid()),
            "a process outside the tree was signalled"
        );
        wait_until_gone(kept.leader);
        let status = common::stdout(&listing.procly_in(&procly, &kept.main, &["status"]));
        assert!(
            !status.contains("service web healthy"),
            "status shows a service healthy next to the stranger holding its port: {status}"
        );
        assert!(
            procly.daemon_log().contains("no longer listened on its Lease"),
            "the Daemon did not say it judged the service gone: {}",
            procly.daemon_log()
        );
    }

    #[test]
    fn a_kept_service_whose_listener_ended_while_a_stranger_took_its_port_is_judged_gone_by_the_daemon_itself() {
        a_stranger_holds_the_port_of_a_kept_service(true);
    }

    #[test]
    fn up_does_not_answer_reused_for_a_kept_service_while_a_stranger_holds_its_port() {
        a_stranger_holds_the_port_of_a_kept_service(false);
    }

    #[test]
    fn a_kept_service_whose_listener_still_runs_is_judged_again_once_the_process_list_can_be_read_and_stays() {
        let procly = Procly::new();
        let listing = Listing::new(procly.scratch());
        let kept = Kept::up(&procly, &listing);
        restart_unable_to_list(&procly, &listing, &kept, false);

        // While the Daemon cannot list the processes, the document of `up` tells what it cannot tell, and stderr stays
        // empty.
        let doubted = common::json_succeeded(&listing.procly_in(&procly, &kept.main, &["up", "--json", "web"]));
        assert_eq!(
            doubted["services"][0]["outcome"],
            serde_json::json!("reused"),
            "{doubted}"
        );
        let warnings = doubted["services"][0]["warnings"].as_array().expect("a warnings array");
        assert_eq!(warnings.len(), 1, "{doubted}");
        assert!(
            warnings[0]
                .as_str()
                .is_some_and(|text| text.contains("cannot tell that web is what listens on port")),
            "{doubted}"
        );

        listing.restore();
        common::eventually("the Daemon to judge the kept service", || {
            procly.daemon_log().contains("can list the processes again")
        });
        let output = listing.procly_in(&procly, &kept.main, &["up", "web"]);

        assert_success(&output);
        common::assert_declared_port(
            kept.port,
            &common::stdout(&output),
            &format!("web reused http://localhost:{port} port {port}\n", port = kept.port),
        );
        assert_eq!(
            common::stderr(&output),
            "",
            "up still doubted a service that was judged"
        );
        assert_eq!(kept.starts(), 1, "the service was started a second time");
        assert!(!procly.daemon_log().contains("was stopped"), "{}", procly.daemon_log());

        // The listener is part of its tree now, down stops it with the leader.
        assert_success(&listing.procly_in(&procly, &kept.main, &["down", "web"]));
        wait_until_gone(kept.listener);
        wait_until_gone(kept.leader);
    }

    #[test]
    fn a_kept_service_whose_leader_ends_before_any_listing_leaves_its_listener_alone_when_the_process_list_can_be_read()
    {
        let procly = Procly::new();
        let listing = Listing::new(procly.scratch());
        let kept = Kept::up(&procly, &listing);
        restart_unable_to_list(&procly, &listing, &kept, false);

        killpg(kept.leader, Signal::SIGKILL).expect("end the leader");
        common::eventually("the Daemon to see the leader end", || {
            procly.daemon_log().contains("its exit code is unknown")
        });
        listing.restore();

        // Nothing proves the listener was in the session of the leader all along, so the Daemon takes the session id of
        // a leader it never saw run for nobody's, and signals nothing: the listener keeps its port.
        std::thread::sleep(Duration::from_millis(1500));
        assert!(
            common::is_alive(kept.listener),
            "a process of a session nobody could prove was the service's was signalled: {}",
            procly.daemon_log()
        );
        let status = common::stdout(&listing.procly_in(&procly, &kept.main, &["status"]));
        assert!(
            !status.contains("service web healthy"),
            "status shows a service that ended: {status}"
        );
    }

    /// What happens to the list of processes between the moment the Daemon re-attaches the Service and the moment its
    /// leader ends.
    #[derive(Debug, Clone, Copy)]
    enum Order {
        /// Refused at the re-attach, back once the Daemon has seen the leader end.
        RefusedThenBack,
        /// Refused at the re-attach, back, and the leader ends at once.
        BackThenEnds,
        /// Refused at the re-attach and never back.
        StaysRefused,
        /// Refused at the re-attach, back for longer than the Daemon takes between 2 looks at the Services it judges,
        /// with nothing asking for the tree, then the leader ends.
        BackAndWaitedThenEnds,
        /// Refused at the re-attach, back and read by a `status`, then the leader ends.
        BackAndReadThenEnds,
        /// Never refused.
        Readable,
    }

    /// A Service whose listener runs in the group of its leader, kept by a Daemon that restarted. Its leader ends before
    /// any reading of the tree could have shown it running, in the order `order`. Whatever the order, the leader's group
    /// is the Service's to stop: `down` leaves no process and no listener behind.
    fn the_group_of_the_leader_of_a_kept_service_is_stopped_whatever_the_list_did(order: Order) {
        let procly = Procly::new();
        let listing = Listing::new(procly.scratch());
        let kept = Kept::up_with(&procly, &listing, Listener::LeaderGroup);
        procly.kill_daemon();
        if !matches!(order, Order::Readable) {
            listing.refuse();
        }
        listing.start_daemon(&procly, &kept.main);
        let saw_the_leader_end = || procly.daemon_log().contains("its exit code is unknown");
        match order {
            Order::BackThenEnds => listing.restore(),
            Order::BackAndWaitedThenEnds => {
                listing.restore();
                // Nothing in the Daemon reads the tree of a Service kept with its listener in the group of its leader.
                std::thread::sleep(Duration::from_millis(1500));
            }
            Order::BackAndReadThenEnds => {
                listing.restore();
                assert_success(&listing.procly_in(&procly, &kept.main, &["status"]));
            }
            Order::RefusedThenBack | Order::StaysRefused | Order::Readable => {}
        }

        // Only the leader ends: its group, which holds the listener, is the Service's to stop.
        nix::sys::signal::kill(kept.leader, Signal::SIGKILL).expect("end the leader");
        common::eventually("the Daemon to see the leader end", saw_the_leader_end);
        if matches!(order, Order::RefusedThenBack) {
            listing.restore();
        }
        assert_success(&listing.procly_in(&procly, &kept.main, &["down", "web"]));

        common::eventually_within(
            Duration::from_secs(5),
            &format!("the listener of the group of the leader to end ({order:?})"),
            || !common::is_alive(kept.listener),
            || procly.daemon_log(),
        );
        assert!(
            std::net::TcpStream::connect(("127.0.0.1", kept.port)).is_err(),
            "the listener still holds its port after down ({order:?}): {}",
            procly.daemon_log()
        );
    }

    #[test]
    fn the_group_of_a_leader_that_ends_is_stopped_when_the_list_came_back_only_after_it_or_never() {
        the_group_of_the_leader_of_a_kept_service_is_stopped_whatever_the_list_did(Order::RefusedThenBack);
        the_group_of_the_leader_of_a_kept_service_is_stopped_whatever_the_list_did(Order::StaysRefused);
    }

    #[test]
    fn the_group_of_a_leader_that_ends_is_stopped_when_the_list_came_back_before_it_and_nothing_read_the_tree() {
        the_group_of_the_leader_of_a_kept_service_is_stopped_whatever_the_list_did(Order::BackThenEnds);
        the_group_of_the_leader_of_a_kept_service_is_stopped_whatever_the_list_did(Order::BackAndWaitedThenEnds);
    }

    #[test]
    fn the_group_of_a_leader_that_ends_is_stopped_when_the_tree_was_read_or_the_list_never_refused() {
        the_group_of_the_leader_of_a_kept_service_is_stopped_whatever_the_list_did(Order::BackAndReadThenEnds);
        the_group_of_the_leader_of_a_kept_service_is_stopped_whatever_the_list_did(Order::Readable);
    }
}
