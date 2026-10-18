//! The CPU time used by a process group, read with `ps` which macOS and Linux both ship.

use std::io;
use std::process::Command;
use std::time::Duration;

use nix::unistd::Pid;

/// The CPU time all the live members of the process group `pgid` have used so far. A member that ended takes
/// its share with it, so this can go down.
pub(super) fn group_cpu(pgid: Pid) -> io::Result<Duration> {
    let output = Command::new("ps").args(["-A", "-o", "pgid=,time="]).output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!("ps exited with {}", output.status)));
    }
    Ok(sum_of_group(&String::from_utf8_lossy(&output.stdout), pgid))
}

/// Adds up the CPU time of the lines of a `ps -o pgid=,time=` listing that belong to `pgid`.
fn sum_of_group(listing: &str, pgid: Pid) -> Duration {
    listing
        .lines()
        .filter_map(|line| {
            let (group, time) = line.trim().split_once(char::is_whitespace)?;
            (group.parse::<i32>().ok()? == pgid.as_raw()).then(|| parse_time(time.trim()))?
        })
        .sum()
}

/// Reads a `ps` cumulative CPU time: `[[DD-]HH:]MM:SS` on Linux, with minutes that may pass 59 and hundredths of
/// a second on the seconds on macOS.
fn parse_time(text: &str) -> Option<Duration> {
    let (days, clock) = match text.split_once('-') {
        Some((days, clock)) => (days.parse::<u64>().ok()?, clock),
        None => (0, text),
    };
    let mut seconds = days * 86_400;
    let mut fields = clock.rsplit(':');
    let last = Duration::try_from_secs_f64(fields.next()?.parse().ok()?).ok()?;
    // Not a zip: it would take one field too many from `fields` before noticing the units ran out.
    for unit in [60, 3600] {
        let Some(field) = fields.next() else { break };
        seconds += field.parse::<u64>().ok()? * unit;
    }
    if fields.next().is_some() {
        return None;
    }
    Some(Duration::from_secs(seconds) + last)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_time_macos_prints() {
        assert_eq!(parse_time("0:00.03"), Some(Duration::from_millis(30)));
        assert_eq!(
            parse_time("239:36.13"),
            Some(Duration::from_millis(239 * 60_000 + 36_130))
        );
    }

    #[test]
    fn reads_the_time_linux_prints() {
        assert_eq!(parse_time("00:00:07"), Some(Duration::from_secs(7)));
        assert_eq!(parse_time("01:02:03"), Some(Duration::from_secs(3723)));
        assert_eq!(parse_time("2-00:00:01"), Some(Duration::from_secs(2 * 86_400 + 1)));
    }

    #[test]
    fn rejects_what_is_not_a_time() {
        assert_eq!(parse_time(""), None);
        assert_eq!(parse_time("-"), None);
        assert_eq!(parse_time("1:xx"), None);
        assert_eq!(parse_time("1:2:3:4"), None);
    }

    #[test]
    fn adds_up_the_lines_of_the_group_only() {
        let listing =
            "    1 239:36.13\n  500   0:01.50\n  500   0:00.25\n 5000   9:00.00\n\ngarbage\n  500 not-a-time\n";
        assert_eq!(sum_of_group(listing, Pid::from_raw(500)), Duration::from_millis(1750));
        assert_eq!(sum_of_group(listing, Pid::from_raw(42)), Duration::ZERO);
    }

    #[test]
    fn measures_a_live_group() {
        // The test process leads no group of its own under the harness, so read the group it belongs to.
        let cpu = group_cpu(nix::unistd::getpgrp()).expect("ps runs");
        assert!(cpu > Duration::ZERO);
    }
}
