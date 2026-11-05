//! `procly daemon stop`: stops the Daemon of the state directory from the outside, without speaking the protocol.
//!
//! The protocol checks its version before anything else, so a Daemon left by another version of Procly answers
//! every request with an error. A signal reaches it whatever it speaks, which is what an upgrade needs.

use std::fs::{self, File, TryLockError};
use std::io;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use nix::errno::Errno;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use serde::Serialize;

use crate::state_dir::StateDir;

/// How long `daemon stop` waits for the Daemon to go, twice the time the Daemon gives one Service to stop, since it
/// stops its Services in parallel and may need that time for a Service and again for the Jobs.
const STOP_WAIT: Duration = Duration::from_secs(10);
/// How often `daemon stop` looks again.
const POLL: Duration = Duration::from_millis(50);

/// What `daemon stop` found and did, told apart by `outcome`.
#[derive(Debug, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub(crate) enum DaemonStop {
    /// No Daemon held the lock of the state directory, whatever the pid file said.
    NotRunning {
        /// Always `null`: no Daemon, no pid.
        pid: Option<i32>,
    },
    /// The Daemon was signalled and its socket went.
    Stopped { pid: i32, waited_ms: u64 },
}

/// Stops the Daemon of `state` with `SIGTERM` and waits for it to go, or reports that none runs.
///
/// Never sends `SIGKILL`: a Daemon that has not gone after [`STOP_WAIT`] is an error the caller reports.
pub(crate) fn run(state: &StateDir) -> anyhow::Result<DaemonStop> {
    if !runs(state)? {
        return Ok(DaemonStop::NotRunning { pid: None });
    }
    let start = Instant::now();
    // The lock is taken before the pid is written, so a Daemon that has just started may not have written it yet.
    let pid = loop {
        if let Some(pid) = read_pid(state) {
            break pid;
        }
        if !runs(state)? {
            return Ok(DaemonStop::NotRunning { pid: None });
        }
        if start.elapsed() >= STOP_WAIT {
            bail!(
                "the Daemon holds {} but {} holds no pid",
                state.lock().display(),
                state.pid().display()
            );
        }
        std::thread::sleep(POLL);
    };
    match kill(Pid::from_raw(pid), Signal::SIGTERM) {
        // Gone between the lock and the signal, the wait below finds the lock free.
        Ok(()) | Err(Errno::ESRCH) => {}
        Err(errno) => return Err(errno).with_context(|| format!("cannot signal the Daemon (pid {pid})")),
    }
    // The Daemon removes its socket, then its pid file, and releases the lock as it exits. Waiting for the lock too
    // is what lets the next command start a Daemon that will not find the lock still held.
    while state.socket().exists() || runs(state)? {
        if start.elapsed() >= STOP_WAIT {
            bail!("the Daemon (pid {pid}) did not stop within {} s", STOP_WAIT.as_secs());
        }
        std::thread::sleep(POLL);
    }
    Ok(DaemonStop::Stopped {
        pid,
        waited_ms: u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
    })
}

/// Whether a Daemon runs, read from its lock rather than from the pid file, whose pid another process may have
/// reused after the Daemon crashed.
fn runs(state: &StateDir) -> anyhow::Result<bool> {
    let path = state.lock();
    let lock = match File::open(&path) {
        Ok(lock) => lock,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).with_context(|| format!("cannot open the daemon lock {}", path.display())),
    };
    // A shared lock conflicts with the exclusive one of the Daemon and is let go at once, as `lock` is dropped.
    match lock.try_lock_shared() {
        Ok(()) => Ok(false),
        Err(TryLockError::WouldBlock) => Ok(true),
        Err(TryLockError::Error(error)) => {
            Err(error).with_context(|| format!("cannot test the daemon lock {}", path.display()))
        }
    }
}

/// The pid the Daemon wrote, when the file holds one.
fn read_pid(state: &StateDir) -> Option<i32> {
    let pid: i32 = fs::read_to_string(state.pid()).ok()?.trim().parse().ok()?;
    (pid > 0).then_some(pid)
}
