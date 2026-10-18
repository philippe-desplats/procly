//! Signals to the process group of a Job or a Service, which reach its whole tree.

use std::time::Duration;

use nix::errno::Errno;
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;

/// How long a stopped tree gets between `SIGTERM` and `SIGKILL`.
pub(super) const STOP_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the output of an ended Job or Service is still read, when a descendant that left its group keeps it
/// open.
pub(super) const DRAIN_TIMEOUT: Duration = Duration::from_secs(1);
/// How many times a group is sent `SIGKILL` before giving up on members that keep appearing.
const KILL_ATTEMPTS: u32 = 100;

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
