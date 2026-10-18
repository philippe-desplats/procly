//! The clock behind every delay that depends on the passing of time: restart backoff, idle stop and Lease expiry.
//!
//! Tests cannot wait minutes, so the Daemon reads `PROCLY_TEST_CLOCK` once at startup. When it names a file, time
//! stands still until the test writes the reading of the clock in it, as a number of milliseconds, and every wait
//! ends once the file shows that much time has passed. A missing or unreadable file reads as 0. Replace the file
//! rather than writing into it in place, so the Daemon never reads half a number.
//!
//! Waits for a health check, a stop or the draining of output are about the real world, not about the passing of
//! time, and stay on real time.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use tokio::time::Instant;

/// Names the file holding the reading of the test clock, in milliseconds. Only tests set it.
pub(crate) const TEST_CLOCK_ENV: &str = "PROCLY_TEST_CLOCK";

/// How often a wait on the test clock reads the file again.
const TEST_POLL: Duration = Duration::from_millis(10);

#[derive(Debug, Clone)]
pub(crate) struct Clock {
    source: Source,
}

#[derive(Debug, Clone)]
enum Source {
    Real(Instant),
    File(PathBuf),
}

impl Clock {
    /// The clock of this Daemon: the test clock when [`TEST_CLOCK_ENV`] is set, real time otherwise.
    pub(crate) fn from_env() -> Self {
        match std::env::var_os(TEST_CLOCK_ENV).filter(|path| !path.is_empty()) {
            Some(path) => Self::file(PathBuf::from(path)),
            None => Self::real(),
        }
    }

    pub(crate) fn real() -> Self {
        Self {
            source: Source::Real(Instant::now()),
        }
    }

    pub(crate) fn file(path: PathBuf) -> Self {
        Self {
            source: Source::File(path),
        }
    }

    /// The time passed since the clock started, which only compares with another reading of the same clock.
    pub(crate) fn now(&self) -> Duration {
        match &self.source {
            Source::Real(start) => start.elapsed(),
            Source::File(path) => read(path),
        }
    }

    /// The time passed since the Unix epoch, which still compares with a reading taken before the Daemon restarted.
    /// On the test clock, the reading of its file.
    pub(crate) fn since_epoch(&self) -> Duration {
        match &self.source {
            // A system clock set before 1970 reads as the epoch itself.
            Source::Real(_) => SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default(),
            Source::File(path) => read(path),
        }
    }

    /// Waits until `duration` has passed on this clock.
    pub(crate) async fn sleep(&self, duration: Duration) {
        match &self.source {
            Source::Real(_) => tokio::time::sleep(duration).await,
            Source::File(path) => {
                let until = read(path).saturating_add(duration);
                while read(path) < until {
                    tokio::time::sleep(TEST_POLL).await;
                }
            }
        }
    }
}

/// The reading of the test clock file, 0 when it is missing or does not hold a number of milliseconds.
fn read(path: &Path) -> Duration {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| text.trim().parse().ok())
        .map_or(Duration::ZERO, Duration::from_millis)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_test_clock_reads_milliseconds_from_its_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clock");
        let clock = Clock::file(path.clone());
        assert_eq!(clock.now(), Duration::ZERO);
        std::fs::write(&path, "1500\n").unwrap();
        assert_eq!(clock.now(), Duration::from_millis(1500));
        assert_eq!(clock.since_epoch(), Duration::from_millis(1500));
        std::fs::write(&path, "soon").unwrap();
        assert_eq!(clock.now(), Duration::ZERO);
    }

    #[tokio::test]
    async fn a_wait_on_the_test_clock_ends_once_the_file_shows_enough_time() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clock");
        std::fs::write(&path, "1000").unwrap();
        let clock = Clock::file(path.clone());
        let waiting = tokio::spawn({
            let clock = clock.clone();
            async move { clock.sleep(Duration::from_secs(60)).await }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!waiting.is_finished(), "the wait ended without time passing");
        std::fs::write(&path, "61000").unwrap();
        tokio::time::timeout(Duration::from_secs(5), waiting)
            .await
            .expect("the wait ends once a minute has passed")
            .unwrap();
    }
}
