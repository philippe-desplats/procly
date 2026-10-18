//! Signals to the process group of a Job or a Service, which reach its whole tree.

use std::process::Command;
use std::time::Duration;

use nix::errno::Errno;
use nix::sys::signal::{Signal, kill, killpg};
use nix::unistd::Pid;

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
    /// When the process started, as `ps` prints it, to the second.
    pub(super) started: String,
}

/// The identity of the live process `pid`, read with `ps`, which macOS and Linux both ship. `None` when no such
/// process exists or `ps` cannot tell.
///
/// The locale and the time zone are fixed, so that a Daemon started from a shell and one started by the session
/// manager read the same start time.
pub(super) fn identity(pid: Pid) -> Option<Identity> {
    let output = Command::new("ps")
        .args(["-o", "pgid=,lstart=", "-p", &pid.to_string()])
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .output()
        .ok()
        .filter(|output| output.status.success())?;
    parse_identity(&String::from_utf8_lossy(&output.stdout))
}

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
    fn reads_no_identity_from_an_empty_or_partial_line() {
        assert_eq!(parse_identity(""), None);
        assert_eq!(parse_identity("4242"), None);
        assert_eq!(parse_identity("4242 "), None);
        assert_eq!(parse_identity("pgid Sun Oct 18 20:52:01 2026"), None);
    }
}
