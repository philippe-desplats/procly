//! Who sent a request, for the Daemon log: there is no per-session ownership of Services (ADR 0010), so the log
//! is how a developer learns which agent stopped a Service another one was using.

use std::path::Path;
use std::process::Command;

/// The requesting command and its parent, which is usually the agent or the shell that ran it, by pid and name.
pub(super) fn describe(pid: Option<i32>, cwd: &Path) -> String {
    let Some(pid) = pid else {
        return format!("an unknown process in {}", cwd.display());
    };
    let Some((parent_pid, name)) = parent_and_name(pid) else {
        return format!("pid {pid}, already gone, in {}", cwd.display());
    };
    let parent = match parent_and_name(parent_pid) {
        Some((_, parent_name)) => format!("parent pid {parent_pid} ({parent_name})"),
        None => format!("parent pid {parent_pid}"),
    };
    format!("pid {pid} ({name}) in {}, {parent}", cwd.display())
}

/// The parent pid and the executable name of a process, read with `ps`, which macOS and Linux both ship.
fn parent_and_name(pid: i32) -> Option<(i32, String)> {
    let output = Command::new("ps")
        .args(["-o", "ppid=,comm=", "-p", &pid.to_string()])
        .output()
        .ok()
        .filter(|output| output.status.success())?;
    let line = String::from_utf8_lossy(&output.stdout);
    let (parent_pid, command) = line.trim().split_once(char::is_whitespace)?;
    // macOS prints the full path of the executable, Linux its name alone.
    let name = command.trim().rsplit('/').next()?.to_owned();
    Some((parent_pid.parse().ok()?, name))
}
