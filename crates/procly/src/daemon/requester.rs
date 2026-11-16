//! Who sent a request, for the Daemon log: there is no per-session ownership of Services (ADR 0010), so the log
//! is how a developer learns which agent stopped a Service another one was using.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::{Command, Stdio};

use super::listeners;

/// The requesting command `pid` and every process it descends from, such as the agent and the shell that ran it, read
/// with `ps`, so that `up` never tells the requester to stop itself. Only `pid` when the processes cannot be listed.
/// Blocking.
pub(super) fn lineage(pid: i32) -> BTreeSet<i32> {
    let mut command = Command::new("ps");
    command
        .args(["-A", "-o", "pid=,ppid="])
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    let parents = match listeners::bounded_output(&mut command) {
        Ok(Some(output)) => parse_parents(&String::from_utf8_lossy(&output.stdout)),
        Ok(None) | Err(_) => BTreeMap::new(),
    };
    ancestry(pid, &parents)
}

/// The parent of each process of `ps -o pid=,ppid=` output.
fn parse_parents(output: &str) -> BTreeMap<i32, i32> {
    output
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            Some((fields.next()?.parse().ok()?, fields.next()?.parse().ok()?))
        })
        .collect()
}

/// `pid` and its ancestors by `parents`, up to a process with no parent.
fn ancestry(pid: i32, parents: &BTreeMap<i32, i32>) -> BTreeSet<i32> {
    let mut lineage = BTreeSet::from([pid]);
    let mut current = pid;
    while let Some(&parent) = parents.get(&current) {
        // A pid seen already would loop, as `ps` lists the kernel's process 0 as its own parent on Linux.
        if parent <= 0 || !lineage.insert(parent) {
            break;
        }
        current = parent;
    }
    lineage
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lineage_of_a_requester_is_itself_and_every_process_it_descends_from() {
        let parents = parse_parents("    1     0\n  300     1\n  400   300\n  500   400\n  600   300\n");
        assert_eq!(ancestry(500, &parents), BTreeSet::from([1, 300, 400, 500]));
        assert_eq!(ancestry(600, &parents), BTreeSet::from([1, 300, 600]));
    }

    #[test]
    fn the_lineage_of_a_requester_ps_cannot_list_is_the_requester_alone() {
        assert_eq!(ancestry(500, &BTreeMap::new()), BTreeSet::from([500]));
        // A loop in what `ps` printed ends the walk.
        let parents = parse_parents("0 0\n7 8\n8 7\n");
        assert_eq!(ancestry(7, &parents), BTreeSet::from([7, 8]));
        assert_eq!(ancestry(0, &parents), BTreeSet::from([0]));
    }
}
