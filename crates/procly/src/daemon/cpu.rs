//! The CPU time used by a process group, read with `ps` which macOS and Linux both ship.

use super::process_group::Pid;
use std::io;
use std::process::Command;
use std::time::Duration;

/// CPU time as `ps` reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct CpuTime {
    /// The CPU time used so far.
    pub(super) used: Duration,
    /// The step `ps` counts CPU time in, a hundredth of a second on macOS and a whole second on Linux. `ps` drops
    /// what lies below it, so 2 readings one step apart may be a mere instant apart. Zero when no process was read.
    pub(super) step: Duration,
}

/// The CPU time all the live members of the process group `pgid` have used so far. A member that ended takes
/// its share with it, so this can go down.
pub(super) fn group_cpu(pgid: Pid) -> io::Result<CpuTime> {
    let output = Command::new("ps").args(["-A", "-o", "pgid=,time="]).output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!("ps exited with {}", output.status)));
    }
    sum_of_group(&String::from_utf8_lossy(&output.stdout), pgid)
        .ok_or_else(|| io::Error::other("cannot read the CPU time of a member of the group in the ps listing"))
}

/// Adds up the CPU time of the lines of a `ps -o pgid=,time=` listing that belong to `pgid`, with the coarsest step
/// among them. `None` when one of those lines has a time that cannot be read, as a sum that skips it would read low
/// and look idle.
fn sum_of_group(listing: &str, pgid: Pid) -> Option<CpuTime> {
    let mut total = CpuTime {
        used: Duration::ZERO,
        step: Duration::ZERO,
    };
    for line in listing.lines() {
        let Some((group, time)) = line.trim().split_once(char::is_whitespace) else {
            continue;
        };
        if group.parse::<i32>().ok() == Some(pgid.as_raw()) {
            let time = parse_time(time.trim())?;
            total.used += time.used;
            total.step = total.step.max(time.step);
        }
    }
    Some(total)
}

/// Reads a `ps` cumulative CPU time: `[[DD-]HH:]MM:SS` on Linux, with minutes that may pass 59 and hundredths of
/// a second on the seconds on macOS. Its step is the unit of the last digit of the seconds.
fn parse_time(text: &str) -> Option<CpuTime> {
    let (days, clock) = match text.split_once('-') {
        Some((days, clock)) => (days.parse::<u64>().ok()?, clock),
        None => (0, text),
    };
    let mut seconds = days * 86_400;
    let mut fields = clock.rsplit(':');
    let last_field = fields.next()?;
    let last = Duration::try_from_secs_f64(last_field.parse().ok()?).ok()?;
    let decimals = last_field.split_once('.').map_or(0, |(_, decimals)| decimals.len());
    let step = Duration::from_nanos(1_000_000_000 / 10_u64.pow(u32::try_from(decimals.min(9)).ok()?));
    // Not a zip: it would take one field too many from `fields` before noticing the units ran out.
    for unit in [60, 3600] {
        let Some(field) = fields.next() else { break };
        seconds += field.parse::<u64>().ok()? * unit;
    }
    if fields.next().is_some() {
        return None;
    }
    Some(CpuTime {
        used: Duration::from_secs(seconds) + last,
        step,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const HUNDREDTH: Duration = Duration::from_millis(10);
    const SECOND: Duration = Duration::from_secs(1);

    fn time(used: Duration, step: Duration) -> CpuTime {
        CpuTime { used, step }
    }

    #[test]
    fn reads_the_time_macos_prints_in_hundredths_of_a_second() {
        assert_eq!(parse_time("0:00.03"), Some(time(Duration::from_millis(30), HUNDREDTH)));
        assert_eq!(
            parse_time("239:36.13"),
            Some(time(Duration::from_millis(239 * 60_000 + 36_130), HUNDREDTH))
        );
    }

    #[test]
    fn reads_the_time_linux_prints_in_whole_seconds() {
        assert_eq!(parse_time("00:00:07"), Some(time(Duration::from_secs(7), SECOND)));
        assert_eq!(parse_time("01:02:03"), Some(time(Duration::from_secs(3723), SECOND)));
        assert_eq!(
            parse_time("2-00:00:01"),
            Some(time(Duration::from_secs(2 * 86_400 + 1), SECOND))
        );
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
        let listing = "    1 239:36.13\n  500   0:01.50\n  500   0:00.25\n 5000   9:00.00\n\ngarbage\n";
        assert_eq!(
            sum_of_group(listing, Pid::from_raw(500)),
            Some(time(Duration::from_millis(1750), HUNDREDTH))
        );
        assert_eq!(
            sum_of_group(listing, Pid::from_raw(42)),
            Some(time(Duration::ZERO, Duration::ZERO))
        );
    }

    #[test]
    fn gives_no_sum_when_a_member_of_the_group_has_an_unreadable_time() {
        let listing = "  500 0:01.50\n  500 not-a-time\n";
        assert_eq!(sum_of_group(listing, Pid::from_raw(500)), None);
    }
}
