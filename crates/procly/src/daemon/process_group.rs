//! The Unix side of the spawn and stop boundary (docs/rules/processes.md): process groups of Jobs and Services,
//! the signals that reach their whole tree, and how their leader ended.
//!
//! Every other module goes through this interface and never calls the system itself, so that a Windows port adds a
//! `#[cfg(windows)]` sibling of this module with the same interface, built on Job Objects, and changes nothing else.

use std::os::unix::process::ExitStatusExt as _;
use std::process::ExitStatus;
use std::time::Duration;

use nix::errno::Errno;
use nix::sys::signal::{Signal, kill, killpg};
use nix::sys::wait::{WaitStatus, waitpid};
/// The id of a process or of a process group, which every module names processes by.
pub(super) use nix::unistd::Pid;
use process_wrap::tokio::{ChildWrapper, CommandWrap, ProcessGroup};
use tokio::task::JoinHandle;

use crate::protocol::{JobEnd, ServiceEnd};

/// How long a stopped tree gets between `SIGTERM` and `SIGKILL`.
pub(super) const STOP_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the output of an ended Job or Service is still read, when a descendant that left its group keeps it
/// open.
pub(super) const DRAIN_TIMEOUT: Duration = Duration::from_secs(1);
/// How many times a group is sent `SIGKILL` before giving up on members that keep appearing.
const KILL_ATTEMPTS: u32 = 100;
/// How often a stopped group that is not a child of the Daemon is looked at, to tell whether it is gone.
const GONE_POLL: Duration = Duration::from_millis(20);

/// Sends `SIGKILL` to the group until it is empty, so that a member forked while the first signal was in flight
/// goes too. The group keeps its id reserved while any member lives, so this never reaches an unrelated process.
pub(super) fn kill_group(pgid: Pid) {
    for _ in 0..KILL_ATTEMPTS {
        match killpg(pgid, Signal::SIGKILL) {
            Ok(()) => std::thread::sleep(Duration::from_millis(10)),
            Err(Errno::ESRCH) => return,
            Err(error) => {
                eprintln!("cannot kill process group {pgid}: {error}");
                return;
            }
        }
    }
    eprintln!("process group {pgid} still has members after {KILL_ATTEMPTS} SIGKILL");
}

pub(super) fn signal_group(pgid: Pid, signal: Signal) {
    match killpg(pgid, signal) {
        Ok(()) | Err(Errno::ESRCH) => {}
        Err(error) => eprintln!("cannot send {signal} to process group {pgid}: {error}"),
    }
}

/// What tells one process from another that later got the same pid: its process group and when it started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Identity {
    pub(super) pgid: Pid,
    /// When the process started, in a form that only compares with another reading on the same host.
    pub(super) started: String,
}

/// The identity of the live process `pid`, `None` when no such process exists or its start cannot be read.
///
/// Linux keeps the start in `/proc` as clock ticks after boot, which stay the same when the wall clock is set. Its
/// `ps` would compute a date from them that moves with the clock, and a Service would then no longer be recognized.
/// macOS keeps the date itself, which `ps` prints, with the locale and the time zone fixed so that a Daemon started
/// from a shell and one started by the session manager read the same.
#[cfg(target_os = "linux")]
pub(super) fn identity(pid: Pid) -> Option<Identity> {
    parse_proc_stat(&std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)
}

/// See the Linux version above.
#[cfg(not(target_os = "linux"))]
pub(super) fn identity(pid: Pid) -> Option<Identity> {
    let output = std::process::Command::new("ps")
        .args(["-o", "pgid=,lstart=", "-p", &pid.to_string()])
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .output()
        .ok()
        .filter(|output| output.status.success())?;
    parse_identity(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(any(target_os = "linux", test))]
/// Reads the process group and the start of `/proc/<pid>/stat`, the 5th and 22nd fields, counted after the name of
/// the command, which may hold spaces and parentheses itself.
fn parse_proc_stat(stat: &str) -> Option<Identity> {
    let (_, fields) = stat.rsplit_once(')')?;
    // The state is the 3rd field, so the 5th is 2 after it and the 22nd 19 after it.
    let fields: Vec<&str> = fields.split_whitespace().collect();
    let pgid = fields.get(2)?.parse().ok()?;
    let started: u64 = fields.get(19)?.parse().ok()?;
    Some(Identity {
        pgid: Pid::from_raw(pgid),
        started: format!("{started} ticks after boot"),
    })
}

#[cfg(any(not(target_os = "linux"), test))]
/// Reads the line `ps -o pgid=,lstart=` prints, such as `4242 Sun Oct 18 20:52:01 2026`.
fn parse_identity(line: &str) -> Option<Identity> {
    let (pgid, started) = line.trim().split_once(char::is_whitespace)?;
    let started = started.trim();
    if started.is_empty() {
        return None;
    }
    Some(Identity {
        pgid: Pid::from_raw(pgid.parse().ok()?),
        started: started.to_owned(),
    })
}

/// Whether the process `pid` still exists. One that is not a child of the Daemon is reaped by init as it ends.
pub(super) fn is_alive(pid: Pid) -> bool {
    !matches!(kill(pid, None), Err(Errno::ESRCH))
}

/// Whether the process group `pgid` still has a member.
pub(super) fn group_exists(pgid: Pid) -> bool {
    !matches!(killpg(pgid, None), Err(Errno::ESRCH))
}

/// Stops a process group that is not a child of the Daemon, so that it cannot be waited for: `SIGTERM` to the whole
/// group, then `SIGKILL` once [`STOP_TIMEOUT`] has passed with members left, and returns once it is empty.
pub(super) async fn stop_group(pgid: Pid) {
    signal_group(pgid, Signal::SIGTERM);
    let deadline = tokio::time::Instant::now() + STOP_TIMEOUT;
    while group_exists(pgid) && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(GONE_POLL).await;
    }
    // Sending signals sleeps between attempts, which must not park a runtime worker.
    let _ = tokio::task::spawn_blocking(move || kill_group(pgid)).await;
}

/// The process group of the Daemon itself, whose members Procly never counts as Foreign processes.
pub(super) fn own_group() -> Pid {
    nix::unistd::getpgrp()
}

/// Makes the spawned command, which serves a Service without belonging to its tree, lead a process group of its own,
/// so that stopping the Service never signals it and it outlives the Daemon.
pub(super) fn detach(command: &mut tokio::process::Command) {
    command.process_group(0);
}

/// Makes the spawned command lead a process group of its own, so that a signal to the group reaches its whole tree.
pub(super) fn lead_group(command: &mut CommandWrap) {
    command.wrap(ProcessGroup::leader());
}

/// Stops the leader of the process group of a Service: `SIGTERM` to the whole group, then `SIGKILL` after the stop
/// timeout.
pub(super) async fn stop_tree(child: &mut dyn ChildWrapper) {
    if let Err(error) = child.signal(Signal::SIGTERM as i32) {
        eprintln!("cannot send SIGTERM to process group {:?}: {error}", child.id());
    }
    if tokio::time::timeout(STOP_TIMEOUT, child.wait()).await.is_err() {
        if let Err(error) = child.start_kill() {
            eprintln!("cannot kill process group {:?}: {error}", child.id());
        }
        let _ = child.wait().await;
    }
}

/// How the leader of a Service ended, from its exit status.
pub(super) fn service_end(status: ExitStatus) -> ServiceEnd {
    match status.signal() {
        Some(signal) => ServiceEnd::Signaled { signal },
        None => ServiceEnd::Exited {
            code: status.code().expect("an exit status without a signal carries a code"),
        },
    }
}

/// Reaps the leader of a Job with `waitpid`, which tells an exit code from a signal, unlike `portable_pty::Child`,
/// and tells how it ended. Blocking.
pub(super) fn wait_for_job(pid: Pid) -> JobEnd {
    loop {
        match waitpid(pid, None) {
            Ok(WaitStatus::Signaled(_, signal, _)) => return JobEnd::Signaled { signal: signal as i32 },
            Ok(WaitStatus::Exited(_, code)) => return JobEnd::Exited { code },
            Ok(_) | Err(Errno::EINTR) => {}
            Err(error) => panic!("cannot wait for job process {pid}, which only the daemon reaps: {error}"),
        }
    }
}

/// Stops the leader of the process group of a Job, reaped by `waiter`: `SIGTERM` to the whole group, then `SIGKILL`
/// after the stop timeout.
pub(super) async fn stop_job_tree(pgid: Pid, mut waiter: JoinHandle<JobEnd>) {
    signal_group(pgid, Signal::SIGTERM);
    if tokio::time::timeout(STOP_TIMEOUT, &mut waiter).await.is_err() {
        signal_group(pgid, Signal::SIGKILL);
        let _ = waiter.await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_group_and_the_start_time_ps_prints() {
        assert_eq!(
            parse_identity(" 4242 Sun Oct 18 20:52:01 2026\n"),
            Some(Identity {
                pgid: Pid::from_raw(4242),
                started: "Sun Oct 18 20:52:01 2026".to_owned()
            })
        );
    }

    #[test]
    fn reads_the_group_and_the_start_time_linux_keeps_in_proc() {
        // The name of the command may hold spaces and parentheses, the fields after its last one are the numbers.
        let stat = "4242 (my (odd) cmd) S 1 4242 4242 0 -1 4194560 120 0 0 0 1 2 0 0 20 0 1 0 987654 1234 56 \
                    18446744073709551615\n";
        assert_eq!(
            parse_proc_stat(stat),
            Some(Identity {
                pgid: Pid::from_raw(4242),
                started: "987654 ticks after boot".to_owned()
            })
        );
        assert_eq!(parse_proc_stat("4242 (cmd) S 1"), None);
    }

    #[test]
    fn reads_no_identity_from_an_empty_or_partial_line() {
        assert_eq!(parse_identity(""), None);
        assert_eq!(parse_identity("4242"), None);
        assert_eq!(parse_identity("4242 "), None);
        assert_eq!(parse_identity("pgid Sun Oct 18 20:52:01 2026"), None);
    }
}
