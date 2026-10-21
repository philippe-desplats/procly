//! `procly gc`: lists the Orphans of every Repository, what removed Worktrees left behind, and deletes them only when
//! asked to (ADR 0004).
//!
//! An Orphan is something whose Worktree `git worktree list` no longer lists, prunable entries included, and that no
//! Pinned branch holds. Nothing of a Repository whose Worktrees git cannot list is one, nor anything of a Worktree
//! where a Service still runs or a setup command still runs.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::lease::WorktreeLease;
use super::worktree_dir::{self, WorktreeDir};
use super::{Registry, answer, blocking, requester};
use crate::protocol::{GcReport, Orphan, OrphanKind, Response, SkippedRepository};
use crate::topology;

/// Answers a `Gc` request, logging who asked for every deletion.
pub(super) async fn run(registry: &Arc<Registry>, cwd: PathBuf, apply: bool, peer: Option<i32>) -> Response {
    let registry = Arc::clone(registry);
    // Listing the Worktrees runs git, naming the requester runs ps, and the rest reads and writes the disk.
    answer(
        blocking("the orphans could not be collected", move || {
            collect(&registry, &cwd, apply, peer)
        })
        .await,
        Response::Gc,
    )
}

fn collect(registry: &Registry, cwd: &Path, apply: bool, peer: Option<i32>) -> anyhow::Result<GcReport> {
    let leases = registry.leases.bound_to_worktrees()?;
    let outputs = worktree_dir::list(registry.services.outputs().dir());
    let setups = worktree_dir::list(&registry.setup_logs);

    let repositories: BTreeSet<PathBuf> = leases
        .iter()
        .map(|lease| lease.repository.clone())
        .chain(
            outputs
                .iter()
                .chain(&setups)
                .filter_map(|dir| Some(dir.owner.as_ref()?.repository.clone())),
        )
        .collect();
    let mut listed = BTreeMap::new();
    let mut skipped = Vec::new();
    for repository in repositories {
        match topology::listed_roots(&repository) {
            Ok(roots) => {
                listed.insert(repository, roots);
            }
            Err(error) => skipped.push(SkippedRepository {
                repository,
                error: format!("{:#}", anyhow::Error::from(error)),
            }),
        }
    }
    let in_use = in_use(registry);
    let removed = |repository: &Path, worktree: &Path| {
        listed.get(repository).is_some_and(|roots| !roots.contains(worktree)) && !in_use.contains(worktree)
    };

    let mut orphans: Vec<Orphan> = leases
        .into_iter()
        .filter(|lease| removed(&lease.repository, &lease.worktree))
        .map(lease_orphan)
        .collect();
    for (dirs, kind) in [
        (
            outputs,
            (|path| OrphanKind::OutputDirectory { path }) as fn(PathBuf) -> OrphanKind,
        ),
        (setups, |path| OrphanKind::SetupDirectory { path }),
    ] {
        orphans.extend(dirs.into_iter().filter_map(|WorktreeDir { path, owner }| {
            let owner = owner?;
            removed(&owner.repository, &owner.worktree).then(|| Orphan {
                kind: kind(path.clone()),
                repository: owner.repository,
                worktree: owner.worktree,
                deleted: false,
                error: None,
            })
        }));
    }

    if apply && !orphans.is_empty() {
        let who = requester::describe(peer, cwd);
        eprintln!("gc --apply of {} orphans requested by {who}", orphans.len());
        for orphan in &mut orphans {
            match delete(registry, orphan) {
                Ok(()) => {
                    orphan.deleted = true;
                    eprintln!("gc deleted {}, requested by {who}", orphan.describe());
                }
                Err(error) => {
                    let error = format!("{:#}", anyhow::Error::from(error));
                    eprintln!("gc cannot delete {}: {error}, requested by {who}", orphan.describe());
                    orphan.error = Some(error);
                }
            }
        }
    }
    Ok(GcReport {
        applied: apply,
        orphans,
        skipped,
    })
}

/// The root of every Worktree where a Service runs, or where `worktree init` runs a setup command: nothing of it is
/// an Orphan while it is in use, even once git no longer lists it.
fn in_use(registry: &Registry) -> BTreeSet<PathBuf> {
    let mut in_use: BTreeSet<PathBuf> = registry.running().into_iter().map(|(worktree, _)| worktree).collect();
    let initializing = registry
        .initializing
        .lock()
        .expect("the init lock table mutex is never poisoned, no code panics while holding it");
    in_use.extend(
        initializing
            .iter()
            .filter(|(_, lock)| lock.try_lock().is_err())
            .map(|(worktree, _)| worktree.clone()),
    );
    in_use
}

fn lease_orphan(lease: WorktreeLease) -> Orphan {
    Orphan {
        kind: OrphanKind::Lease {
            service: lease.service,
            port_name: lease.port_name,
            port: lease.port,
        },
        repository: lease.repository,
        worktree: lease.worktree,
        deleted: false,
        error: None,
    }
}

#[derive(Debug, thiserror::Error)]
enum DeleteError {
    #[error(transparent)]
    Lease(#[from] super::store::StoreError),
    #[error("cannot remove {path}")]
    Remove {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Deletes `orphan`. A directory already gone counts as deleted.
fn delete(registry: &Registry, orphan: &Orphan) -> Result<(), DeleteError> {
    match &orphan.kind {
        OrphanKind::Lease {
            service,
            port_name,
            port,
        } => Ok(registry.leases.release(&WorktreeLease {
            repository: orphan.repository.clone(),
            worktree: orphan.worktree.clone(),
            service: service.clone(),
            port_name: port_name.clone(),
            port: *port,
        })?),
        OrphanKind::OutputDirectory { path } | OrphanKind::SetupDirectory { path } => worktree_dir::remove(path)
            .map_err(|source| DeleteError::Remove {
                path: path.clone(),
                source,
            }),
    }
}
