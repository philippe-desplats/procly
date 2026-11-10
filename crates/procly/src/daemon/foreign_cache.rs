//! What the Daemon last found about Foreign processes, so that `Status` does not look at the whole system with `lsof`
//! and `docker ps` on every call.
//!
//! `Status` answers from a [`Sighting`] begun at most [`FRESH_FOR`] before it was asked, and looks itself when the last
//! one is older. While `Status` is asked, a task looks again every [`LOOK_EVERY`], so that a client polling every
//! 2 s gets its answer without waiting for a look, and it stops [`KEEP_LOOKING`] after the last `Status`: nothing looks
//! at the system for a Daemon nobody asks. The paths that name what holds a port, the port diagnostic and Doctor,
//! read the system themselves and never go through it.
//!
//! A look that panics finds nothing, as a look without `lsof` does, and the next one waits [`LOOK_EVERY`], then twice
//! as long after each new failure up to [`LONGEST_WAIT`], so that a look that keeps failing never loads the Daemon nor
//! floods its log. The Daemon logs a failure once, and again only when its message changes or after a look succeeded.

use std::any::Any;
use std::panic::{self, AssertUnwindSafe};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use super::foreign::Sighting;
use super::{Registry, foreign, status};

/// How long after it began a [`Sighting`] may answer `Status`: a Foreign process shows up in `Status` at most this
/// long after it starts listening, and leaves it at most this long after it ends. Stated in `docs/daemon.md`.
pub(super) const FRESH_FOR: Duration = Duration::from_secs(2);
/// How often the Daemon looks again while `Status` is asked, well under [`FRESH_FOR`] so that a look slowed down by
/// the load still ends before the last one is too old to answer.
const LOOK_EVERY: Duration = Duration::from_secs(1);
/// How long after the last `Status` the Daemon keeps looking: longer than the 2 s the desktop app polls at with its
/// window in front, shorter than the 10 s it polls at behind another window.
const KEEP_LOOKING: Duration = Duration::from_secs(3);
/// The longest wait after looks that keep failing.
const LONGEST_WAIT: Duration = Duration::from_secs(30);
/// Names a file while which exists every look panics, as a bug in it would. Only tests set it.
const TEST_LOOK_PANICS_ENV: &str = "PROCLY_TEST_LOOK_PANICS";

/// The last [`Sighting`] of the Daemon, with when the look that made it began.
#[derive(Debug, Default)]
pub(super) struct ForeignCache {
    latest: Mutex<Option<(Instant, Arc<Sighting>)>>,
    /// Held while looking, so that the task and the `Status` calls never look at once, and a `Status` that finds the
    /// last Sighting too old waits for the look already running rather than starting another.
    looking: Mutex<()>,
    /// When `Status` was last asked.
    asked: Mutex<Option<Instant>>,
    /// Whether the task that keeps looking runs.
    keeping: AtomicBool,
    /// The last failed look, while no look has succeeded since.
    failure: Mutex<Option<Failure>>,
}

/// A look that panicked, and how long the next one waits.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Failure {
    at: Instant,
    wait: Duration,
    message: String,
}

impl ForeignCache {
    /// A Sighting begun at most [`FRESH_FOR`] before now, made with `look` when the last one is older. An empty one
    /// while the last look failed and its wait is not over. Blocking, as looking is.
    pub(super) fn sighting(&self, look: impl FnOnce() -> Sighting) -> Arc<Sighting> {
        let asked = Instant::now();
        *lock(&self.asked) = Some(asked);
        if let Some(sighting) = self.fresh(asked) {
            return sighting;
        }
        let _looking = lock(&self.looking);
        // The look this call waited for may have begun after it was asked.
        if let Some(sighting) = self.fresh(asked) {
            return sighting;
        }
        if self.waiting() {
            return Arc::default();
        }
        self.look(look)
    }

    /// Looks again with `look` unless a look began less than [`LOOK_EVERY`] ago, as one a `Status` made, or the last
    /// one failed and its wait is not over. Blocking.
    fn refresh(&self, look: impl FnOnce() -> Sighting) {
        let _looking = lock(&self.looking);
        if self.began().is_some_and(|began| began.elapsed() < LOOK_EVERY) || self.waiting() {
            return;
        }
        self.look(look);
    }

    /// When the task looks next: [`LOOK_EVERY`] after the last look began, and not before the wait of a failure.
    fn next_look(&self) -> Instant {
        let after_last = self.began().map_or_else(Instant::now, |began| began + LOOK_EVERY);
        lock(&self.failure)
            .as_ref()
            .map_or(after_last, |failure| after_last.max(failure.at + failure.wait))
    }

    /// Whether the last look failed and the wait it set is not over.
    fn waiting(&self) -> bool {
        lock(&self.failure)
            .as_ref()
            .is_some_and(|failure| failure.at.elapsed() < failure.wait)
    }

    /// Records that a look panicked with `message`, doubling the wait of the previous failure, and returns the line to
    /// log, `None` when the previous failure already logged that message.
    fn failed(&self, message: String) -> Option<String> {
        let mut failure = lock(&self.failure);
        let previous = failure.take();
        let wait = previous
            .as_ref()
            .map_or(LOOK_EVERY, |previous| (previous.wait * 2).min(LONGEST_WAIT));
        let line = (previous.as_ref().map(|previous| &previous.message) != Some(&message))
            .then(|| format!("cannot look for Foreign processes, trying again in {wait:?} at most: {message}"));
        *failure = Some(Failure {
            at: Instant::now(),
            wait,
            message,
        });
        line
    }

    /// The last Sighting, if it began at most [`FRESH_FOR`] before `asked`.
    fn fresh(&self, asked: Instant) -> Option<Arc<Sighting>> {
        lock(&self.latest)
            .as_ref()
            .filter(|(began, _)| asked.saturating_duration_since(*began) <= FRESH_FOR)
            .map(|(_, sighting)| Arc::clone(sighting))
    }

    /// Makes a Sighting with `look` and keeps it. The caller holds `looking`, which a look that panics leaves
    /// released and unpoisoned: the panic is caught here, and the look finds nothing.
    fn look(&self, look: impl FnOnce() -> Sighting) -> Arc<Sighting> {
        let began = Instant::now();
        match panic::catch_unwind(AssertUnwindSafe(look)) {
            Ok(sighting) => {
                let sighting = Arc::new(sighting);
                *lock(&self.latest) = Some((began, Arc::clone(&sighting)));
                *lock(&self.failure) = None;
                sighting
            }
            Err(payload) => {
                if let Some(line) = self.failed(panic_message(payload.as_ref())) {
                    eprintln!("{line}");
                }
                Arc::default()
            }
        }
    }

    fn began(&self) -> Option<Instant> {
        lock(&self.latest).as_ref().map(|(began, _)| *began)
    }

    /// Whether `Status` was asked within the last [`KEEP_LOOKING`].
    fn wanted(&self) -> bool {
        lock(&self.asked).is_some_and(|asked| asked.elapsed() <= KEEP_LOOKING)
    }
}

/// Starts the task that looks again every [`LOOK_EVERY`] while `Status` is asked, unless it runs. Called after each
/// `Status`, from the runtime.
pub(super) fn keep_looking(registry: &Arc<Registry>) {
    if registry.foreign.keeping.swap(true, Ordering::AcqRel) {
        return;
    }
    let registry = Arc::clone(registry);
    tokio::spawn(async move {
        loop {
            tokio::time::sleep_until(registry.foreign.next_look().into()).await;
            if !registry.foreign.wanted() {
                registry.foreign.keeping.store(false, Ordering::Release);
                // A `Status` asked in between found the task running and started none.
                if !registry.foreign.wanted() || registry.foreign.keeping.swap(true, Ordering::AcqRel) {
                    return;
                }
            }
            let looking = Arc::clone(&registry);
            // `lsof` and `docker ps` are blocking. A look that panics is caught by `look`, which sets the wait of
            // the next one, so the task never turns without sleeping.
            if tokio::task::spawn_blocking(move || looking.foreign.refresh(|| sight(&looking)))
                .await
                .is_err()
            {
                // Only a runtime shutting down cancels the look.
                registry.foreign.keeping.store(false, Ordering::Release);
                return;
            }
        }
    });
}

/// Looks at the system, with what the Services, Jobs and containers of `registry` run. Blocking.
pub(super) fn sight(registry: &Registry) -> Sighting {
    assert!(
        !std::env::var_os(TEST_LOOK_PANICS_ENV).is_some_and(|path| Path::new(&path).exists()),
        "{TEST_LOOK_PANICS_ENV} makes every look panic"
    );
    foreign::sight(|| status::owned(registry))
}

/// The message a panic carries, as `panic!` and `expect` set it.
fn panic_message(payload: &(dyn Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|message| (*message).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "a panic without a message".to_owned())
}

/// Locks `mutex`, even poisoned: each value it guards is replaced whole, never left half written, so a panic while it
/// was held leaves a value as good as any other.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    #[test]
    fn a_status_answers_from_a_sighting_younger_than_its_bound_and_looks_again_once_it_is_older() {
        let cache = ForeignCache::default();
        let looks = Cell::new(0);
        let look = || {
            looks.set(looks.get() + 1);
            Sighting::default()
        };

        cache.sighting(look);
        cache.sighting(look);
        assert_eq!(looks.get(), 1, "the second Status answers from the first look");

        let older = Instant::now()
            .checked_sub(FRESH_FOR + Duration::from_millis(1))
            .expect("an instant in the past");
        lock(&cache.latest).as_mut().expect("a Sighting is kept").0 = older;
        cache.sighting(look);
        assert_eq!(looks.get(), 2, "a Sighting older than its bound is never an answer");
    }

    #[test]
    fn the_task_looks_again_only_once_the_last_look_is_old_enough() {
        let cache = ForeignCache::default();
        let looks = Cell::new(0);
        let look = || {
            looks.set(looks.get() + 1);
            Sighting::default()
        };

        cache.refresh(look);
        cache.refresh(look);
        assert_eq!(
            looks.get(),
            1,
            "a look that just began, as one a Status made, is not repeated"
        );

        let older = Instant::now().checked_sub(LOOK_EVERY).expect("an instant in the past");
        lock(&cache.latest).as_mut().expect("a Sighting is kept").0 = older;
        cache.refresh(look);
        assert_eq!(looks.get(), 2);
    }

    #[test]
    fn the_cache_is_wanted_only_shortly_after_a_status() {
        let cache = ForeignCache::default();
        assert!(!cache.wanted(), "no Status was asked yet");

        cache.sighting(Sighting::default);
        assert!(cache.wanted());

        *lock(&cache.asked) = Instant::now().checked_sub(KEEP_LOOKING + Duration::from_millis(1));
        assert!(!cache.wanted());
    }

    /// Moves the wait of the last failure into the past, as if it were over.
    fn end_the_wait(cache: &ForeignCache) {
        let mut failure = lock(&cache.failure);
        let failure = failure.as_mut().expect("a failure is kept");
        failure.at = Instant::now()
            .checked_sub(failure.wait)
            .expect("an instant in the past");
    }

    #[test]
    fn a_look_that_panics_finds_nothing_and_the_next_status_answers_once_its_wait_is_over() {
        let cache = ForeignCache::default();
        let looks = Cell::new(0);
        let look = || {
            looks.set(looks.get() + 1);
            Sighting::default()
        };

        let found = cache.sighting(|| panic!("lsof said something unexpected"));
        let worktrees = [std::path::PathBuf::from("/src/app")];
        assert_eq!(found.in_worktrees(&worktrees, &foreign::Owned::default()), [Vec::new()]);
        assert!(
            cache.looking.try_lock().is_ok(),
            "the lock is released and not poisoned"
        );

        // Within the wait, a Status answers without looking, and the task does not look either.
        cache.sighting(look);
        cache.refresh(look);
        assert_eq!(looks.get(), 0);
        assert!(cache.next_look() >= lock(&cache.failure).as_ref().expect("a failure is kept").at + LOOK_EVERY);

        end_the_wait(&cache);
        cache.sighting(look);
        assert_eq!(looks.get(), 1, "the wait over, the next Status looks again");
        assert_eq!(*lock(&cache.failure), None, "a look that succeeds ends the failure");
    }

    #[test]
    fn looks_that_keep_failing_log_once_and_wait_twice_as_long_each_time() {
        let cache = ForeignCache::default();
        let mut logged = Vec::new();
        let mut waits = Vec::new();
        for _ in 0..8 {
            logged.extend(cache.failed("lsof said something unexpected".to_owned()));
            waits.push(lock(&cache.failure).as_ref().expect("a failure is kept").wait);
        }

        assert_eq!(
            logged,
            ["cannot look for Foreign processes, trying again in 1s at most: lsof said something unexpected"]
        );
        let seconds: Vec<u64> = waits.iter().map(Duration::as_secs).collect();
        assert_eq!(seconds, [1, 2, 4, 8, 16, 30, 30, 30]);

        // Another failure is logged once too.
        assert!(cache.failed("docker said something unexpected".to_owned()).is_some());
        assert!(cache.failed("docker said something unexpected".to_owned()).is_none());
    }
}
