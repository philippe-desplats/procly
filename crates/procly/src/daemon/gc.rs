//! `procly gc`: lists the Orphans of every Repository, what removed Worktrees left behind, and deletes them only when
//! asked to (ADR 0004).
//!
//! An Orphan is something whose Worktree `git worktree list` no longer lists, prunable entries included, and that no
//! Pinned branch holds. Nothing of a Repository whose Worktrees git cannot list is one, nor anything of a Worktree
//! where a Service still runs or a setup command still runs.
//!
//! A Logical resource is dropped in its container through the `drop` command of its Driver, and only then forgotten
//! and its index freed, so that an index is never granted again while it still holds the data of a removed Worktree.
//! Whether it is still an Orphan is worked out again with its container held, since a Worktree added while `gc` runs
//! may use it by then.
//!
//! An Isolated container is an Orphan only when Procly recorded it, and Docker lists it under the compose project
//! and service of its record with the removed Worktree as its working directory. Its volumes are the named volumes
//! of that project it mounts, never a volume the compose file gives a name of its own.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use super::compose::{DropError, Dropped, ToDrop};
use super::isolated_record::IsolatedRecord;
use super::lease::WorktreeLease;
use super::resource::{LogicalResource, ResourceKey};
use super::store::StoreError;
use super::worktree_dir::{self, WorktreeDir};
use super::{Registry, blocking, foreign, listeners, read_config, requester};
use crate::config::{Config, Settings};
use crate::protocol::{GcReport, Orphan, OrphanKind, Response, SkippedRepository};
use crate::topology::{self, TopologyError};

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
        let mut orphans = Vec::new();
        for mut orphan in std::mem::take(&mut collected.report.orphans) {
            let timeout = collected
                .driver_timeouts
                .get(&orphan.repository)
                .copied()
                .unwrap_or_else(|| Settings::default().driver_timeout);
            match delete(registry, &collected.found, &orphan, timeout).await {
                Ok(Dropped::Dropped) => {
                    orphan.deleted = true;
                    eprintln!("gc deleted {}, requested by {who}", orphan.describe());
                }
                // No longer an Orphan, so no longer reported as one.
                Ok(Dropped::Held) => {
                    eprintln!(
                        "gc kept {}, a Worktree uses it again, requested by {who}",
                        orphan.describe()
                    );
                    continue;
                }
                Err(error) => {
                    let error = format!("{:#}", anyhow::Error::from(error));
                    eprintln!("gc cannot delete {}: {error}, requested by {who}", orphan.describe());
                    orphan.error = Some(error);
                }
            }
            orphans.push(orphan);
        }
        collected.report.orphans = orphans;
    }
    if apply {
        let (registry, found, orphans) = (
            Arc::clone(registry),
            Arc::clone(&collected.found),
            collected.report.orphans.clone(),
        );
        // Writing the document of the Isolated containers is blocking.
        if let Err(error) = tokio::task::spawn_blocking(move || forget_removed(&registry, &found, &orphans)).await {
            eprintln!("cannot update the records of the Isolated containers gc removed: {error}");
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
    /// The Isolated containers of removed Worktrees, as Docker listed them.
    found: Arc<Vec<Found>>,
}

fn collect(registry: &Registry, cwd: &Path, apply: bool, peer: Option<i32>) -> anyhow::Result<Collected> {
    let leases = registry.leases.bound_to_worktrees()?;
    let resources = registry.resources.all()?;
    let outputs = worktree_dir::list(registry.services.outputs().dir());
    let setups = worktree_dir::list(&registry.setup_logs);
    let isolated = registry.isolated.all()?;

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
        .chain(isolated.iter().map(|record| record.repository.clone()))
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
        let roots = listed.get(&resource.key.repository);
        let index = registry.indexes.get(&resource.key)?;
        if held(&resource, index, config, roots) {
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

    // A Pinned branch holds its container whichever Worktree checks it out next, as it holds its Leases.
    let found: Vec<Found> = isolated
        .into_iter()
        .filter(|record| record.pinned.is_none() && removed(&record.repository, &record.worktree))
        .map(find)
        .collect();
    orphans.extend(isolated_orphans(&found));

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
        found: Arc::new(found),
    })
}

/// The Isolated containers `found` lists, then their volumes.
fn isolated_orphans(found: &[Found]) -> Vec<Orphan> {
    let mut orphans: Vec<Orphan> = found
        .iter()
        .filter(|found| found.is_listed())
        .map(|found| Orphan {
            kind: OrphanKind::IsolatedContainer {
                service: found.record.service.clone(),
                project: found.record.project.clone(),
                compose: found.record.compose.clone(),
            },
            repository: found.record.repository.clone(),
            worktree: found.record.worktree.clone(),
            deleted: false,
            error: None,
        })
        .collect();
    // After every container, which holds its volumes until it is removed. A volume 2 containers of the project mount
    // is listed once.
    let mut volumes = BTreeSet::new();
    for found in found {
        for volume in &found.volumes {
            if volumes.insert((&found.record.project, volume)) {
                orphans.push(Orphan {
                    kind: OrphanKind::IsolatedVolume {
                        project: found.record.project.clone(),
                        volume: volume.clone(),
                    },
                    repository: found.record.repository.clone(),
                    worktree: found.record.worktree.clone(),
                    deleted: false,
                    error: None,
                });
            }
        }
    }
    orphans
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
/// configuration `config` of the Main worktree pins it or cannot be read, for an index, by nothing it can be matched
/// with, such as an index handed out again after the index document was removed, its key holding `index` now, and for
/// a name, by a Worktree git
/// still lists among `roots` whose directory has the same name, since a Driver that names its resources names them
/// after the Worktree slug alone and such a Worktree uses the same resource.
fn held(
    resource: &LogicalResource,
    index: Option<u32>,
    config: Option<&Config>,
    roots: Option<&BTreeSet<PathBuf>>,
) -> bool {
    if let Some(branch) = &resource.branch
        && config.is_none_or(|config| config.pinned.contains_key(branch))
    {
        return true;
    }
    if resource.indexed {
        return index.is_none_or(|index| index.to_string() != resource.name);
    }
    // A declared name holds no double underscore, so the slug is what follows the first one. A name without one is
    // not a name the Daemon gave a Worktree, and is left alone.
    let Some((_, slug)) = resource.name.split_once("__") else {
        return true;
    };
    roots.is_some_and(|roots| roots.iter().any(|root| topology::slug_of(root) == slug))
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

/// An Isolated container whose Worktree was removed, as Docker lists it now.
#[derive(Debug)]
struct Found {
    record: IsolatedRecord,
    /// The names of the containers of the record, `None` when Docker did not answer.
    containers: Option<Vec<String>>,
    /// The named volumes of its project it mounts, and those a previous `gc` could not remove, that still exist.
    volumes: Vec<String>,
}

impl Found {
    /// Whether the container is an Orphan to list: Docker lists it, or could not tell. One Docker no longer lists is
    /// only a record left, which `gc --apply` forgets.
    fn is_listed(&self) -> bool {
        self.containers.as_ref().is_none_or(|containers| !containers.is_empty())
    }
}

/// Looks the container of `record` and its volumes up in Docker. Blocking.
fn find(record: IsolatedRecord) -> Found {
    let containers = owned_containers(&record);
    let mut volumes = record.volumes.clone();
    for container in containers.iter().flatten() {
        for volume in mounted_volumes(container, &record.env) {
            if !volumes.contains(&volume) {
                volumes.push(volume);
            }
        }
    }
    let of_project = project_volumes(&record.project, &record.env);
    volumes.retain(|volume| of_project.contains(volume));
    Found {
        record,
        containers,
        volumes,
    }
}

/// The containers Docker lists, running or not, as the one of the compose service of `record` in its project, and
/// whose working directory is the Worktree of `record`, so that a project of another Repository under the same name
/// is never touched. `None` when Docker did not answer. Blocking.
fn owned_containers(record: &IsolatedRecord) -> Option<Vec<String>> {
    let labels = [
        format!("label=com.docker.compose.project={}", record.project),
        format!("label=com.docker.compose.service={}", record.compose),
    ];
    let listed = foreign::containers(
        &["--all", "--filter", &labels[0], "--filter", &labels[1]],
        Some(&record.env),
    )?;
    let worktree = resolved(&record.worktree);
    Some(
        listed
            .into_iter()
            .filter(|container| {
                container.is_service(&record.project, &record.compose)
                    && container
                        .working_dir
                        .as_deref()
                        .is_some_and(|dir| resolved(dir) == worktree)
            })
            .map(|container| container.name)
            .collect(),
    )
}

/// The names of the volumes `container` mounts, anonymous ones included. Blocking.
fn mounted_volumes(container: &str, env: &[(String, String)]) -> Vec<String> {
    let format = r#"{{range .Mounts}}{{if eq .Type "volume"}}{{.Name}}{{"\n"}}{{end}}{{end}}"#;
    lines(&["container", "inspect", "--format", format, container], env)
}

/// The named volumes compose created for the project `project`: labelled with it and named after it, which a volume
/// the compose file gives a name of its own is not. Blocking.
fn project_volumes(project: &str, env: &[(String, String)]) -> BTreeSet<String> {
    let label = format!("label=com.docker.compose.project={project}");
    let format = "{{.Name}}\t{{.Label \"com.docker.compose.volume\"}}";
    lines(&["volume", "ls", "--filter", &label, "--format", format], env)
        .into_iter()
        .filter_map(|line| {
            let (name, key) = line.split_once('\t')?;
            (name == format!("{project}_{key}")).then(|| name.to_owned())
        })
        .collect()
}

/// The lines `docker args` prints, none when it fails or does not answer. Blocking.
fn lines(args: &[&str], env: &[(String, String)]) -> Vec<String> {
    let mut command = docker(args, env);
    command.stderr(Stdio::null());
    match listeners::bounded_output(&mut command) {
        Ok(Some(output)) if output.status.success() => String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

/// `docker args` in `env`, the environment that reaches the engine the container runs on.
fn docker(args: &[&str], env: &[(String, String)]) -> Command {
    let mut command = Command::new("docker");
    command
        .args(args)
        .env_clear()
        .envs(env.iter().map(|(name, value)| (name, value)))
        .stdin(Stdio::null());
    command
}

/// Runs `docker args` to remove something, which counts as removed when the engine no longer has it. Blocking.
fn remove(args: &[&str], env: &[(String, String)]) -> Result<(), DeleteError> {
    let failed = |reason: String| DeleteError::Docker {
        args: args.join(" "),
        reason,
    };
    let output = docker(args, env)
        .output()
        .map_err(|error| failed(format!("cannot run docker: {error}")))?;
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if output.status.success() || stderr.to_ascii_lowercase().contains("no such ") {
        return Ok(());
    }
    Err(failed(stderr))
}

/// `path` with its symbolic links resolved, through its parent when it no longer exists, as Worktree roots are.
fn resolved(path: &Path) -> PathBuf {
    std::fs::canonicalize(path)
        .ok()
        .or_else(|| Some(std::fs::canonicalize(path.parent()?).ok()?.join(path.file_name()?)))
        .unwrap_or_else(|| path.to_owned())
}

/// Updates the record of each container `gc --apply` found: forgotten once it and its volumes are removed, keeping
/// the volumes it could not remove otherwise. A container that could not be removed keeps its record as it is.
fn forget_removed(registry: &Registry, found: &[Found], orphans: &[Orphan]) {
    for found in found {
        let record = &found.record;
        let of_record = |orphan: &&Orphan| orphan.repository == record.repository && orphan.worktree == record.worktree;
        let container_removed = !found.is_listed() || orphans.iter().filter(of_record).any(|orphan| {
            orphan.deleted
                && matches!(&orphan.kind, OrphanKind::IsolatedContainer { service, .. } if *service == record.service)
        });
        if !container_removed {
            continue;
        }
        let left = found
            .volumes
            .iter()
            .filter(|volume| {
                !orphans.iter().filter(of_record).any(|orphan| {
                    orphan.deleted
                        && matches!(&orphan.kind, OrphanKind::IsolatedVolume { volume: removed, .. } if removed == *volume)
                })
            })
            .cloned()
            .collect();
        if let Err(error) = registry.isolated.removed(&record.key(), left) {
            eprintln!("cannot update the record of an Isolated container gc removed: {error:#}");
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum DeleteError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Topology(#[from] TopologyError),
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
    #[error("Docker did not answer, the container could not be looked up")]
    Unanswered,
    #[error("`docker {args}` failed: {reason}")]
    Docker { args: String, reason: String },
}

/// Deletes `orphan`, a Logical resource with Driver commands that may run for `timeout`, or an Isolated container or
/// volume as `found` holds it. A directory, a container or a volume already gone counts as deleted, as does a resource
/// the Driver no longer finds. A Logical resource that is no longer an Orphan by the time its container is held is
/// left as it is, [`Dropped::Held`].
async fn delete(
    registry: &Arc<Registry>,
    found: &Arc<Vec<Found>>,
    orphan: &Orphan,
    timeout: Duration,
) -> Result<Dropped, DeleteError> {
    if let OrphanKind::LogicalResource {
        service,
        resource,
        name,
        driver,
    } = &orphan.kind
    {
        let key = ResourceKey {
            repository: orphan.repository.clone(),
            worktree: orphan.worktree.clone(),
            service: service.clone(),
            resource: resource.clone(),
        };
        let still = {
            let (registry, key, name) = (Arc::clone(registry), key.clone(), name.clone());
            move || {
                let index = registry.indexes.get(&key)?;
                still_orphan(&registry, &key, &name, index)
            }
        };
        let forget = {
            let (registry, name) = (Arc::clone(registry), name.clone());
            move || forget(&registry, &key, &name)
        };
        let to_drop = ToDrop {
            repository: &orphan.repository,
            service,
            name,
            driver,
        };
        return registry.composes.drop_resource(to_drop, timeout, still, forget).await;
    }
    let (registry, found, orphan) = (Arc::clone(registry), Arc::clone(found), orphan.clone());
    // Writing the documents, removing the directories and running docker are blocking.
    tokio::task::spawn_blocking(move || delete_blocking(&registry, &found, &orphan))
        .await
        .map_err(|error| DeleteError::Blocking(error.to_string()))??;
    Ok(Dropped::Dropped)
}

/// Whether the Logical resource `key`, named `name`, whose key holds the index `index`, is still an Orphan, worked
/// out again from what git lists now, as [`collect`] does. A resource recorded under another name since, or
/// forgotten, is not one. Blocking: it runs git and reads the disk.
fn still_orphan(registry: &Registry, key: &ResourceKey, name: &str, index: Option<u32>) -> Result<bool, DeleteError> {
    let Some(resource) = registry
        .resources
        .all()?
        .into_iter()
        .find(|resource| resource.key == *key && resource.name == name)
    else {
        return Ok(false);
    };
    let roots = topology::listed_roots(&key.repository)?;
    if roots.contains(&key.worktree) || in_use(registry).contains(&key.worktree) {
        return Ok(false);
    }
    let config = main_config(&key.repository);
    Ok(!held(&resource, index, config.as_ref(), Some(&roots)))
}

/// Forgets the dropped Logical resource `key`, named `name`, then frees its index, so that a failure in between leaves
/// an index taken, never a record of an index another Worktree may be granted, and returns `true`. A resource that is
/// no longer an Orphan is left recorded with its index, and `false` returned.
///
/// Worked out again under the lock of the index table: `procly env` and the `up` of a Service process are handed
/// their indexes outside the lock of the container, so a Worktree added again at the same path while the resource
/// was dropped may hold its index again. Git lists such a Worktree before it asks for an index, and one asking once
/// the index is freed is handed it as a new one. Blocking: it runs git and writes the documents.
fn forget(registry: &Registry, key: &ResourceKey, name: &str) -> Result<bool, DeleteError> {
    registry.indexes.free_if(key, |index| {
        if !still_orphan(registry, key, name, index)? {
            return Ok(false);
        }
        registry.resources.forget(key)?;
        Ok(true)
    })
}

/// Deletes `orphan`, anything but a Logical resource, an Isolated container or volume as `found` holds it. Blocking.
fn delete_blocking(registry: &Registry, found: &[Found], orphan: &Orphan) -> Result<(), DeleteError> {
    let of = |service: &str| {
        found.iter().find(|found| {
            found.record.repository == orphan.repository
                && found.record.worktree == orphan.worktree
                && found.record.service == service
        })
    };
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
        OrphanKind::LogicalResource { .. } => unreachable!("a Logical resource is deleted under its container"),
        OrphanKind::OutputDirectory { path } | OrphanKind::SetupDirectory { path } => worktree_dir::remove(path)
            .map_err(|source| DeleteError::Remove {
                path: path.clone(),
                source,
            }),
        OrphanKind::IsolatedContainer { service, .. } => {
            let Some(found) = of(service) else {
                return Ok(());
            };
            let containers = found.containers.as_ref().ok_or(DeleteError::Unanswered)?;
            let mut args = vec!["container", "rm", "--force", "--volumes"];
            args.extend(containers.iter().map(String::as_str));
            remove(&args, &found.record.env)
        }
        OrphanKind::IsolatedVolume { volume, .. } => {
            let env = found
                .iter()
                .find(|found| found.record.repository == orphan.repository && found.volumes.contains(volume))
                .map(|found| found.record.env.as_slice())
                .unwrap_or_default();
            remove(&["volume", "rm", volume], env)
        }
    }
}
