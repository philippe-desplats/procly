//! Idle detection: a Service that printed nothing and used almost no CPU for a while is forgotten, and stopped.

use std::time::Duration;

/// A Service is busy while its tree uses more than this share of one CPU, averaged since it last was busy.
const MAX_IDLE_CPU: f64 = 0.01;

/// What one look at a Service showed.
#[derive(Debug, Clone, Copy)]
pub(super) struct Sample {
    /// The reading of the Daemon clock, which the idle timeout is counted on.
    pub(super) now: Duration,
    /// Real time, which is what CPU time is spent against, however the clock of the Daemon is driven.
    pub(super) real: Duration,
    /// Every byte the Service ever printed.
    pub(super) written: u64,
    /// The CPU time used by the tree of the Service.
    pub(super) cpu: Duration,
}

/// Tells when a Service has been idle for its timeout.
#[derive(Debug)]
pub(super) struct IdleTracker {
    timeout: Duration,
    /// The look at the Service from which it has been idle since.
    since: Sample,
}

impl IdleTracker {
    pub(super) fn new(timeout: Duration, first: Sample) -> Self {
        Self { timeout, since: first }
    }

    /// Takes a new look, and returns whether the Service has been idle for the whole timeout.
    ///
    /// The Service is busy when it printed anything, when its tree uses more than 1% of a CPU averaged since the
    /// last time it was busy, or when its CPU time dropped, as a process that came and went counts as activity.
    /// Averaging over the whole stretch keeps this true to the CPU clock of `ps`, which is as coarse as a second
    /// on Linux.
    pub(super) fn is_idle(&mut self, sample: Sample) -> bool {
        let real = sample.real.saturating_sub(self.since.real);
        let busy = sample.written != self.since.written
            || sample.cpu < self.since.cpu
            || sample.cpu.saturating_sub(self.since.cpu).as_secs_f64() > MAX_IDLE_CPU * real.as_secs_f64();
        if busy {
            self.since = sample;
            return false;
        }
        sample.now.saturating_sub(self.since.now) >= self.timeout
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TIMEOUT: Duration = Duration::from_mins(10);

    fn sample(now_s: u64, real_s: u64, written: u64, cpu_ms: u64) -> Sample {
        Sample {
            now: Duration::from_secs(now_s),
            real: Duration::from_secs(real_s),
            written,
            cpu: Duration::from_millis(cpu_ms),
        }
    }

    #[test]
    fn a_silent_service_with_no_cpu_is_idle_once_the_timeout_has_passed() {
        let mut tracker = IdleTracker::new(TIMEOUT, sample(0, 0, 10, 50));
        assert!(!tracker.is_idle(sample(599, 599, 10, 50)));
        assert!(tracker.is_idle(sample(600, 600, 10, 50)));
    }

    #[test]
    fn anything_printed_restarts_the_count() {
        let mut tracker = IdleTracker::new(TIMEOUT, sample(0, 0, 10, 50));
        assert!(!tracker.is_idle(sample(500, 500, 11, 50)));
        assert!(!tracker.is_idle(sample(1000, 1000, 11, 50)));
        assert!(tracker.is_idle(sample(1100, 1100, 11, 50)));
    }

    #[test]
    fn using_more_than_one_percent_of_a_cpu_restarts_the_count() {
        let mut tracker = IdleTracker::new(TIMEOUT, sample(0, 0, 0, 0));
        // 2 s of CPU in 100 s, 2%.
        assert!(!tracker.is_idle(sample(700, 100, 0, 2000)));
        // 0.5 s of CPU in 100 s since then, 0.5%, which is below the line.
        assert!(!tracker.is_idle(sample(1200, 200, 0, 2500)));
        assert!(tracker.is_idle(sample(1300, 300, 0, 2500)));
    }

    #[test]
    fn cpu_is_measured_against_real_time_whatever_the_clock_says() {
        let mut tracker = IdleTracker::new(TIMEOUT, sample(0, 0, 0, 0));
        // The clock jumped 10 minutes while a busy loop spent 300 ms of CPU in 300 ms of real time.
        assert!(!tracker.is_idle(sample(600, 0, 0, 300)));
    }

    #[test]
    fn a_drop_of_the_cpu_time_counts_as_activity() {
        let mut tracker = IdleTracker::new(TIMEOUT, sample(0, 0, 0, 5000));
        assert!(!tracker.is_idle(sample(700, 700, 0, 100)));
    }
}
