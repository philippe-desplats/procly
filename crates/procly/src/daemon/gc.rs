//! `procly gc`: lists the Orphans of every Repository, what removed Worktrees left behind, and deletes them only when
//! asked to (ADR 0004).
//!
//! An Orphan is something whose Worktree `git worktree list` no longer lists, prunable entries included, and that no
//! Pinned branch holds. Nothing of a Repository whose Worktrees git cannot list is one, nor anything of a Worktree
//! where a Service still runs or a setup command still runs.
//!
//! A Logical resource is dropped in its container through the `drop` command of its Driver, and only then forgotten
//! and its index freed, so that an index is never granted again while it still holds the data of a removed Worktree.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use super::compose::DropError;
use super::lease::WorktreeLease;
use super::resource::{LogicalResource, ResourceKey};
use super::store::StoreError;
use super::worktree_dir::{self, WorktreeDir};
use super::{Registry, blocking, read_config, requester};
use crate::config::{Config, Settings};
use crate::protocol::{GcReport, Orphan, OrphanKind, Response, SkippedRepository};
use crate::topology;

/// What `gc` says when the Orphans could not be collected.
const COLLECT_FAILED: &str = "the orphans could not be collected";

/// Answers a `Gc` request, logging who asked for every deletion.
pub(super) async fn run(registry: &Arc<Registry>, cwd: PathBuf, apply: bool, peer: Option<i32>) -> Response {
    let collected = {
        let registry = Arc::clone(registry);
        // Listing the Worktrees runs git, naming the requester runs ps, and the rest reads the disk.
        blocking(COLLECT_FAILED, move || collect(&registry, &cwd, apply, peer)).await
    };
    let mut collected = match collected {
        Ok(collected) => collected,
        Err(message) => return Response::Error { message },
    };
    if let Some(who) = &collected.requester {
        eprintln!(
            "gc --apply of {} orphans requested by {who}",
            collected.report.orphans.len()
        );
        for orphan in &mut collected.report.orphans {
            let timeout = collected
                .driver_timeouts
                .get(&orphan.repository)
                .copied()
                .unwrap_or_else(|| Settings::default().driver_timeout);
            match delete(registry, orphan, timeout).await {
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
    Response::Gc(collected.report)
}

/// The Orphans `gc` found, and what deleting them takes.
struct Collected {
    report: GcReport,
    /// Who asked, `Some` only when the Orphans are to be deleted.
    requester: Option<String>,
    /// How long one command of a Driver may run, by Repository, as the Main worktree sets it.
    driver_timeouts: BTreeMap<PathBuf, Duration>,
}

fn collect(registry: &Registry, cwd: &Path, apply: bool, peer: Option<i32>) -> anyhow::Result<Collected> {
    let leases = registry.leases.bound_to_worktrees()?;
    let resources = registry.resources.all()?;
    let outputs = worktree_dir::list(registry.services.outputs().dir());
    let setups = worktree_dir::list(&registry.setup_logs);

    let repositories: BTreeSet<PathBuf> = leases
        .iter()
        .map(|lease| lease.repository.clone())
        .chain(resources.iter().map(|resource| resource.key.repository.clone()))
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
    let resources: Vec<LogicalResource> = resources
        .into_iter()
        .filter(|resource| removed(&resource.key.repository, &resource.key.worktree))
        .collect();
    // The configuration of the Main worktree, read once per Repository that has an orphan resource.
    let configs: BTreeMap<PathBuf, Option<Config>> = resources
        .iter()
        .map(|resource| resource.key.repository.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|repository| {
            let config = main_config(&repository);
            (repository, config)
        })
        .collect();
    for resource in resources {
        let config = configs.get(&resource.key.repository).and_then(Option::as_ref);
        if held(registry, &resource, config)? {
            continue;
        }
        orphans.push(resource_orphan(resource));
    }
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

    let requester = (apply && !orphans.is_empty()).then(|| requester::describe(peer, cwd));
    let driver_timeouts = configs
        .into_iter()
        .filter_map(|(repository, config)| Some((repository, config?.settings.driver_timeout)))
        .collect();
    Ok(Collected {
        report: GcReport {
            applied: apply,
            orphans,
            skipped,
        },
        requester,
        driver_timeouts,
    })
}

/// The configuration of the Main worktree of the Repository `repository`, `None` when it cannot be read.
fn main_config(repository: &Path) -> Option<Config> {
    let main = topology::worktrees(repository)
        .ok()?
        .into_iter()
        .find(|worktree| worktree.main)?;
    read_config(&main.path).ok()
}

/// Whether the removed Worktree's `resource` is still held: by the Pinned branch the Worktree was on, while the
/// configuration `config` of the Main worktree pins it or cannot be read, or, for an index, by nothing it can be
/// matched with, such as an index handed out again after the index document was removed.
fn held(registry: &Registry, resource: &LogicalResource, config: Option<&Config>) -> Result<bool, StoreError> {
    if let Some(branch) = &resource.branch
        && config.is_none_or(|config| config.pinned.contains_key(branch))
    {
        return Ok(true);
    }
    if resource.indexed {
        let index = registry.indexes.get(&resource.key)?;
        return Ok(index.is_none_or(|index| index.to_string() != resource.name));
    }
    Ok(false)
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

fn resource_orphan(resource: LogicalResource) -> Orphan {
    let LogicalResource {
        key:
            ResourceKey {
                repository,
                worktree,
                service,
                resource,
            },
        name,
        driver,
        ..
    } = resource;
    Orphan {
        kind: OrphanKind::LogicalResource {
            service,
            resource,
            name,
            driver,
        },
        repository,
        worktree,
        deleted: false,
        error: None,
    }
}

#[derive(Debug, thiserror::Error)]
enum DeleteError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Drop(#[from] DropError),
    #[error("the deletion could not be run: {0}")]
    Blocking(String),
    #[error("cannot remove {path}")]
    Remove {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Deletes `orphan`, a Logical resource with Driver commands that may run for `timeout`. A directory already gone
/// counts as deleted, as does a resource the Driver no longer finds.
async fn delete(registry: &Arc<Registry>, orphan: &Orphan, timeout: Duration) -> Result<(), DeleteError> {
    if let OrphanKind::LogicalResource {
        service, name, driver, ..
    } = &orphan.kind
    {
        registry
            .composes
            .drop_resource(&orphan.repository, service, name, driver, timeout)
            .await?;
    }
    let (registry, orphan) = (Arc::clone(registry), orphan.clone());
    // Writing the documents and removing the directories are blocking.
    tokio::task::spawn_blocking(move || delete_on_disk(&registry, &orphan))
        .await
        .map_err(|error| DeleteError::Blocking(error.to_string()))?
}

/// Deletes what `orphan` leaves on the disk of the state directory.
fn delete_on_disk(registry: &Registry, orphan: &Orphan) -> Result<(), DeleteError> {
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
        OrphanKind::LogicalResource { service, resource, .. } => {
            let key = ResourceKey {
                repository: orphan.repository.clone(),
                worktree: orphan.worktree.clone(),
                service: service.clone(),
                resource: resource.clone(),
            };
            // Forgotten before its index is freed, so that a failure in between leaves an index taken, never a
            // record of an index another Worktree may be granted.
            registry.resources.forget(&key)?;
            Ok(registry.indexes.free(&key)?)
        }
        OrphanKind::OutputDirectory { path } | OrphanKind::SetupDirectory { path } => worktree_dir::remove(path)
            .map_err(|source| DeleteError::Remove {
                path: path.clone(),
                source,
            }),
    }
}
