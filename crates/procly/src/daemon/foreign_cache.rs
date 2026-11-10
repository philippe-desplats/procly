//! What the Daemon last found about Foreign processes, so that `Status` does not look at the whole system with `lsof`
//! and `docker ps` on every call.
//!
//! `Status` answers from a [`Sighting`] begun at most [`FRESH_FOR`] before it was asked, and looks itself when the last
//! one is older. While `Status` is asked, a task looks again every [`LOOK_EVERY`], so that a client polling every
//! 2 s gets its answer without waiting for a look, and it stops [`KEEP_LOOKING`] after the last `Status`: nothing looks
//! at the system for a Daemon nobody asks. The paths that name what holds a port, the port diagnostic and Doctor,
//! read the system themselves and never go through it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
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
}

impl ForeignCache {
    /// A Sighting begun at most [`FRESH_FOR`] before now, made with `look` when the last one is older. Blocking, as
    /// looking is.
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
        self.look(look)
    }

    /// Looks again with `look` unless a look began less than [`LOOK_EVERY`] ago, as one a `Status` made. Blocking.
    fn refresh(&self, look: impl FnOnce() -> Sighting) {
        let _looking = lock(&self.looking);
        if self.began().is_some_and(|began| began.elapsed() < LOOK_EVERY) {
            return;
        }
        self.look(look);
    }

    /// The last Sighting, if it began at most [`FRESH_FOR`] before `asked`.
    fn fresh(&self, asked: Instant) -> Option<Arc<Sighting>> {
        lock(&self.latest)
            .as_ref()
            .filter(|(began, _)| asked.saturating_duration_since(*began) <= FRESH_FOR)
            .map(|(_, sighting)| Arc::clone(sighting))
    }

    /// Makes a Sighting with `look` and keeps it. The caller holds `looking`.
    fn look(&self, look: impl FnOnce() -> Sighting) -> Arc<Sighting> {
        let began = Instant::now();
        let sighting = Arc::new(look());
        *lock(&self.latest) = Some((began, Arc::clone(&sighting)));
        sighting
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
            let next = registry
                .foreign
                .began()
                .map_or_else(Instant::now, |began| began + LOOK_EVERY);
            tokio::time::sleep_until(next.into()).await;
            if !registry.foreign.wanted() {
                registry.foreign.keeping.store(false, Ordering::Release);
                // A `Status` asked in between found the task running and started none.
                if !registry.foreign.wanted() || registry.foreign.keeping.swap(true, Ordering::AcqRel) {
                    return;
                }
            }
            let looking = Arc::clone(&registry);
            // `lsof` and `docker ps` are blocking, and a look that panicked is retried at the next turn.
            let _ = tokio::task::spawn_blocking(move || {
                looking.foreign.refresh(|| sight(&looking));
            })
            .await;
        }
    });
}

/// Looks at the system, with what the Services, Jobs and containers of `registry` run. Blocking.
pub(super) fn sight(registry: &Registry) -> Sighting {
    foreign::sight(|| status::owned(registry))
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .expect("the Foreign process cache mutexes are never poisoned, no code panics while holding them")
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

        let older = Instant::now().checked_sub(FRESH_FOR + Duration::from_millis(1)).expect("an instant in the past");
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
        assert_eq!(looks.get(), 1, "a look that just began, as one a Status made, is not repeated");

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
}
