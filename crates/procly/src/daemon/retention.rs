//! The Retention policy of the Isolated containers: what the Daemon deletes, a container and its volumes, once it finds
//! their Worktree removed. A policy other than keep is a deletion a person declared in `procly.toml`, so the Daemon
//! applies it on its own, and logs each deletion with the policy that caused it (ADR 0004).
//!
//! The Daemon looks for removed Worktrees when it reconciles what a previous Daemon left, and on every Worktree
//! release. A Worktree is removed as `procly gc` tells it: git no longer lists it, nothing runs in it and no Pinned
//! branch holds its container. The moment the Daemon first found it removed is written in the record of the
//! container, which a delayed deletion counts from, so that a restart of the Daemon neither loses it nor runs it early.
//! A Worktree git lists again before the deletion keeps its container, and its delay starts over once removed again.
//! A Worktree of a Repository git cannot list is neither: its container is not deleted and its delay is left as it is.
//!
//! The container and its volumes are found and removed as `gc --apply` does it, so that a policy never deletes what
//! `gc` would not, such as a running container, or one whose compose project another Worktree uses.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context as _;

use super::Registry;
use super::clock::Clock;
use super::gc::{self, Found, Removed};
use super::isolated_record::{IsolatedRecord, Key};
use crate::config::Retention;
use crate::protocol::OrphanKind;

/// The longest a wait for a delayed deletion sleeps before it reads the clock again, so that the time a machine
/// spent asleep, which a monotonic timer may not count, still brings the deletion closer.
const LONGEST_NAP: Duration = Duration::from_mins(5);

/// The delayed deletions the Daemon waits for, and what keeps 2 sweeps from deleting the same container at once.
#[derive(Debug)]
pub(super) struct Retentions {
    clock: Clock,
    /// Held through a sweep.
    sweeping: tokio::sync::Mutex<()>,
    /// When each container a wait is running for is due, so that a sweep never starts a second wait for it.
    waiting: Mutex<BTreeMap<Key, Duration>>,
}

impl Retentions {
    pub(super) fn new(clock: Clock) -> Self {
        Self {
            clock,
            sweeping: tokio::sync::Mutex::default(),
            waiting: Mutex::default(),
        }
    }

    fn waiting(&self) -> std::sync::MutexGuard<'_, BTreeMap<Key, Duration>> {
        self.waiting
            .lock()
            .expect("the retention wait table mutex is never poisoned, no code panics while holding it")
    }
}

/// Applies the Retention policy of every recorded Isolated container whose Worktree is removed: deletes those that
/// are due, and waits for the others. Logs what it cannot do, nobody else is told.
pub(super) async fn sweep(registry: &Arc<Registry>) {
    let _sweeping = registry.retentions.sweeping.lock().await;
    let swept = {
        let registry = Arc::clone(registry);
        // Listing the Worktrees runs git, finding and removing the containers runs docker, and the record is written.
        tokio::task::spawn_blocking(move || sweep_blocking(&registry)).await
    };
    match swept {
        Ok(Ok(later)) => {
            for (key, due) in later {
                wait(registry, key, due);
            }
        }
        Ok(Err(error)) => eprintln!("cannot apply the retention policies of the Isolated containers: {error:#}"),
        Err(error) => eprintln!("cannot apply the retention policies of the Isolated containers: {error}"),
    }
}

/// Waits until the container `key` is due, at `due` since the Unix epoch on the clock, then sweeps again.
fn wait(registry: &Arc<Registry>, key: Key, due: Duration) {
    {
        let mut waiting = registry.retentions.waiting();
        if waiting.get(&key) == Some(&due) {
            return;
        }
        waiting.insert(key.clone(), due);
    }
    let registry = Arc::clone(registry);
    tokio::spawn(async move {
        let clock = &registry.retentions.clock;
        loop {
            let now = clock.since_epoch();
            if now >= due {
                break;
            }
            clock.sleep(due.saturating_sub(now).min(LONGEST_NAP)).await;
        }
        {
            let mut waiting = registry.retentions.waiting();
            if waiting.get(&key) == Some(&due) {
                waiting.remove(&key);
            }
        }
        sweep(&registry).await;
    });
}

/// Marks the containers whose Worktree the Daemon finds removed, deletes those that are due, and returns when each of
/// the others is. Blocking.
fn sweep_blocking(registry: &Registry) -> anyhow::Result<Vec<(Key, Duration)>> {
    let records = registry.isolated.all().context("cannot read the Isolated containers")?;
    let declared: Vec<&IsolatedRecord> = records
        .iter()
        .filter(|record| record.retention != Retention::Keep)
        .collect();
    if declared.is_empty() {
        return Ok(Vec::new());
    }
    let repositories: BTreeSet<_> = declared.iter().map(|record| record.repository.clone()).collect();
    let (worktrees, skipped) = Removed::list(registry, repositories);
    for skipped in &skipped {
        eprintln!(
            "cannot list the Worktrees of {}, the retention policies of its Isolated containers wait: {}",
            skipped.repository.display(),
            skipped.error
        );
    }
    let now = registry.retentions.clock.since_epoch();
    let mut due = BTreeSet::new();
    let mut later = Vec::new();
    for record in declared {
        let Some(delay) = record.retention.delay() else {
            continue;
        };
        let Some(removed) = removed(record, &worktrees) else {
            continue;
        };
        let key = record.key();
        let found = registry
            .isolated
            .found_removed(&key, removed, now)
            .context("cannot record when the Worktree of an Isolated container was found removed")?;
        log_found(record, removed, found);
        let Some(found) = found else { continue };
        let at = found.saturating_add(delay);
        if at <= now {
            due.insert(key);
        } else {
            later.push((key, at));
        }
    }
    if !due.is_empty() {
        let found = gc::orphan_isolated(
            records,
            |repository, worktree| worktrees.is(repository, worktree),
            |record| due.contains(&record.key()),
        );
        delete(registry, &found);
    }
    Ok(later)
}

/// Whether the Worktree of `record` is removed, as `worktrees` lists them. `None` when git could not list the Worktrees
/// of its Repository: whether it is removed is unknown, so the moment it was found removed stays as it is, neither
/// cleared nor set, and nothing of it is deleted.
fn removed(record: &IsolatedRecord, worktrees: &Removed) -> Option<bool> {
    worktrees
        .knows(&record.repository)
        .then(|| record.pinned.is_none() && worktrees.is(&record.repository, &record.worktree))
}

/// Logs that the Worktree of `record`, which is `removed` and whose moment of being found removed is now `found`, was
/// found removed, or is no longer removed.
fn log_found(record: &IsolatedRecord, removed: bool, found: Option<Duration>) {
    let what = format!(
        "the isolated container of service {} of {}",
        record.service,
        record.worktree.display()
    );
    match (record.removed_ms, found) {
        (None, Some(_)) if removed => eprintln!(
            "the Worktree {} is removed, retention policy {} of service {} deletes {what}",
            record.worktree.display(),
            record.retention,
            record.service,
        ),
        (Some(_), None) if !removed => eprintln!(
            "the Worktree {} is no longer removed, retention policy {} of service {} keeps {what}",
            record.worktree.display(),
            record.retention,
            record.service,
        ),
        _ => {}
    }
}

/// Deletes the containers `found` and their volumes, logging each deletion with the policy that caused it, then
/// updates their records as `gc --apply` does. Blocking.
fn delete(registry: &Registry, found: &[Found]) {
    let mut orphans = gc::isolated_orphans(found);
    for orphan in &mut orphans {
        // The container of a volume is the one that mounts it.
        let Some(cause) = found.iter().find(|found| {
            found.record.repository == orphan.repository
                && found.record.worktree == orphan.worktree
                && match &orphan.kind {
                    OrphanKind::IsolatedContainer { service, .. } => found.record.service == *service,
                    OrphanKind::IsolatedVolume { volume, .. } => found.volumes.contains(volume),
                    _ => false,
                }
        }) else {
            continue;
        };
        let policy = format!(
            "retention policy {} of service {}",
            cause.record.retention, cause.record.service
        );
        match gc::delete_blocking(registry, found, orphan) {
            Ok(()) => {
                orphan.deleted = true;
                eprintln!("{policy} deleted {}", orphan.describe());
            }
            Err(error) => {
                let error = format!("{:#}", anyhow::Error::from(error));
                eprintln!("{policy} cannot delete {}: {error}", orphan.describe());
                orphan.error = Some(error);
            }
        }
    }
    gc::forget_removed(registry, found, &orphans);
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn record(repository: &str, worktree: &str) -> IsolatedRecord {
        IsolatedRecord {
            repository: PathBuf::from(repository),
            worktree: PathBuf::from(worktree),
            service: "db".to_owned(),
            compose: "db".to_owned(),
            project: "app-app_login".to_owned(),
            env: Vec::new(),
            pinned: None,
            volumes: Vec::new(),
            retention: Retention::After(Duration::from_hours(24)),
            removed_ms: Some(5_000),
        }
    }

    #[test]
    fn a_worktree_of_a_repository_git_cannot_list_is_neither_removed_nor_listed_again() {
        // git listed the Worktrees of app, and could not list those of api.
        let worktrees = Removed::new(
            BTreeMap::from([(
                PathBuf::from("/src/app/.git"),
                BTreeSet::from([PathBuf::from("/src/app")]),
            )]),
            BTreeSet::new(),
        );

        assert_eq!(
            removed(&record("/src/app/.git", "/src/app-login"), &worktrees),
            Some(true)
        );
        assert_eq!(removed(&record("/src/app/.git", "/src/app"), &worktrees), Some(false));
        // Its pending deletion is left as it is, not started over.
        assert_eq!(removed(&record("/src/api/.git", "/src/api-login"), &worktrees), None);
    }

    #[test]
    fn a_worktree_that_runs_something_or_is_on_a_pinned_branch_is_not_removed() {
        let worktrees = Removed::new(
            BTreeMap::from([(PathBuf::from("/src/app/.git"), BTreeSet::new())]),
            BTreeSet::from([PathBuf::from("/src/app-busy")]),
        );
        let mut pinned = record("/src/app/.git", "/src/app-login");
        pinned.pinned = Some("main".to_owned());

        assert_eq!(
            removed(&record("/src/app/.git", "/src/app-busy"), &worktrees),
            Some(false)
        );
        assert_eq!(removed(&pinned, &worktrees), Some(false));
    }
}
