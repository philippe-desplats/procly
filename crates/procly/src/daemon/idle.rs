//! Idle detection: a Service that printed nothing and used almost no CPU for a while is forgotten, and stopped.

use std::time::Duration;

use super::cpu::CpuTime;

/// A Service is busy while its tree uses more than this share of one CPU, averaged since it last was busy.
const MAX_IDLE_CPU: f64 = 0.01;

/// What one look at a Service showed.
#[derive(Debug, Clone, Copy)]
pub(super) struct Sample {
    /// The reading of the Daemon clock, which the idle timeout is counted on.
    pub(super) now: Duration,
    /// Real time, which is what CPU time is spent against, however the clock of the Daemon is driven.
    pub(super) real: Duration,
    /// The reading of the Daemon clock when the Service last printed, `None` while it never did.
    pub(super) printed_at: Option<Duration>,
    /// The CPU time used by the tree of the Service.
    pub(super) cpu: CpuTime,
}

/// Tells when a Service has been idle for its timeout.
#[derive(Debug)]
pub(super) struct IdleTracker {
    timeout: Duration,
    /// The last look that found the Service using CPU, or the first look.
    busy: Sample,
}

impl IdleTracker {
    /// Counts idleness from the reading `started` of the clock, with `first` as the first measure of the CPU time,
    /// however later than `started` it was taken.
    pub(super) fn new(timeout: Duration, started: Duration, first: Sample) -> Self {
        Self {
            timeout,
            busy: Sample { now: started, ..first },
        }
    }

    /// Takes a new look, and returns whether the Service has been idle for the whole timeout.
    ///
    /// The Service is idle since the later of the last look that found it using CPU and the moment it last printed.
    /// It uses CPU when its tree uses more than 1% of a CPU averaged since the last look that found it doing so, or
    /// when its CPU time dropped, as a process that came and went counts as activity. Averaging over the whole
    /// stretch keeps this true to the CPU clock of `ps`, which is as coarse as a second on Linux, and one step of
    /// that clock is never taken for use, as 2 readings a step apart may be a mere instant apart. Output counts from
    /// when it was printed, on the clock, rather than from the look that sees it: a line printed just before a look
    /// and read just after does not restart the count at a later reading.
    pub(super) fn is_idle(&mut self, sample: Sample) -> bool {
        let real = sample.real.saturating_sub(self.busy.real);
        let step = sample.cpu.step.max(self.busy.cpu.step);
        let used = sample.cpu.used.saturating_sub(self.busy.cpu.used).saturating_sub(step);
        if sample.cpu.used < self.busy.cpu.used || used.as_secs_f64() > MAX_IDLE_CPU * real.as_secs_f64() {
            self.busy = sample;
            return false;
        }
        let since = sample
            .printed_at
            .map_or(self.busy.now, |printed_at| printed_at.max(self.busy.now));
        sample.now.saturating_sub(since) >= self.timeout
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TIMEOUT: Duration = Duration::from_mins(10);
    /// The step of the CPU clock of `ps` on macOS.
    const HUNDREDTH: Duration = Duration::from_millis(10);

    /// A look at clock reading `now_s` and real time `real_ms`, with a Service that last printed at `printed_s` and
    /// used `cpu_ms` of CPU, counted in hundredths of a second.
    fn sample(now_s: u64, real_ms: u64, printed_s: Option<u64>, cpu_ms: u64) -> Sample {
        Sample {
            now: Duration::from_secs(now_s),
            real: Duration::from_millis(real_ms),
            printed_at: printed_s.map(Duration::from_secs),
            cpu: CpuTime {
                used: Duration::from_millis(cpu_ms),
                step: HUNDREDTH,
            },
        }
    }

    #[test]
    fn a_silent_service_with_no_cpu_is_idle_once_the_timeout_has_passed() {
        let mut tracker = IdleTracker::new(TIMEOUT, Duration::ZERO, sample(0, 0, Some(0), 50));
        assert!(!tracker.is_idle(sample(599, 599_000, Some(0), 50)));
        assert!(tracker.is_idle(sample(600, 600_000, Some(0), 50)));
    }

    #[test]
    fn anything_printed_restarts_the_count_from_when_it_was_printed() {
        let mut tracker = IdleTracker::new(TIMEOUT, Duration::ZERO, sample(0, 0, None, 50));
        assert!(!tracker.is_idle(sample(500, 500_000, Some(450), 50)));
        assert!(!tracker.is_idle(sample(1000, 1_000_000, Some(450), 50)));
        assert!(tracker.is_idle(sample(1050, 1_050_000, Some(450), 50)));
    }

    #[test]
    fn idleness_counts_from_the_start_even_when_the_first_measure_comes_after_the_clock_moved() {
        // The Service turned healthy at 0, and ps answered the first look once the clock read 10 minutes.
        let mut tracker = IdleTracker::new(TIMEOUT, Duration::ZERO, sample(600, 300, None, 0));
        assert!(tracker.is_idle(sample(600, 400, None, 0)));
    }

    #[test]
    fn output_printed_before_the_clock_moved_does_not_count_as_printed_after() {
        // The first line of the Service reached the Daemon after its first look, while the clock read 0, and the
        // clock then moved by the whole timeout before the next look.
        let mut tracker = IdleTracker::new(TIMEOUT, Duration::ZERO, sample(0, 0, None, 0));
        assert!(tracker.is_idle(sample(600, 300, Some(0), 0)));
    }

    #[test]
    fn using_more_than_one_percent_of_a_cpu_restarts_the_count() {
        let mut tracker = IdleTracker::new(TIMEOUT, Duration::ZERO, sample(0, 0, None, 0));
        // 2 s of CPU in 100 s, 2%.
        assert!(!tracker.is_idle(sample(700, 100_000, None, 2000)));
        // 0.5 s of CPU in 100 s since then, 0.5%, which is below the line.
        assert!(!tracker.is_idle(sample(1200, 200_000, None, 2500)));
        assert!(tracker.is_idle(sample(1300, 300_000, None, 2500)));
    }

    #[test]
    fn cpu_is_measured_against_real_time_whatever_the_clock_says() {
        let mut tracker = IdleTracker::new(TIMEOUT, Duration::ZERO, sample(0, 0, None, 0));
        // The clock jumped 10 minutes while a busy loop spent 300 ms of CPU in 300 ms of real time.
        assert!(!tracker.is_idle(sample(600, 300, None, 300)));
    }

    #[test]
    fn one_step_of_the_cpu_clock_is_not_taken_for_use() {
        // The last tick of the startup of the Service, read a fraction of a second after the first look, is not 3%
        // of a CPU, it may be any amount of CPU up to a hundredth of a second.
        let mut tracker = IdleTracker::new(TIMEOUT, Duration::ZERO, sample(0, 0, None, 0));
        assert!(tracker.is_idle(sample(600, 300, None, 10)));
        let mut tracker = IdleTracker::new(TIMEOUT, Duration::ZERO, sample(0, 0, None, 0));
        assert!(!tracker.is_idle(sample(600, 300, None, 20)));
    }

    #[test]
    fn a_whole_second_of_the_cpu_clock_of_linux_is_one_step_too() {
        let linux = |now_s, real_ms, cpu_s| Sample {
            cpu: CpuTime {
                used: Duration::from_secs(cpu_s),
                step: Duration::from_secs(1),
            },
            ..sample(now_s, real_ms, None, 0)
        };
        let mut tracker = IdleTracker::new(TIMEOUT, Duration::ZERO, linux(0, 0, 0));
        assert!(tracker.is_idle(linux(600, 2500, 1)));
        let mut tracker = IdleTracker::new(TIMEOUT, Duration::ZERO, linux(0, 0, 0));
        assert!(!tracker.is_idle(linux(600, 2500, 2)));
    }

    #[test]
    fn a_drop_of_the_cpu_time_counts_as_activity() {
        let mut tracker = IdleTracker::new(TIMEOUT, Duration::ZERO, sample(0, 0, None, 5000));
        assert!(!tracker.is_idle(sample(700, 700_000, None, 100)));
    }
}
