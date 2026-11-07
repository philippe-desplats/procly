//! Worktree release: what a Worktree holds in Procly undone before it is removed, its data kept (ADR 0004).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context as _;

use super::{Registry, read_config, requester, retention, topology_of};
use crate::config::IsolationMode;
use crate::driver::{self, Driver};
use crate::protocol::{KeptResource, ReleaseReport};
use crate::topology::{self, Topology};

/// Releases the Worktree holding `path`, logging who asked: stops its Services and Jobs with their whole trees, drops
/// the held environment of every Service of it, lets go of its Compose services, then releases its Leases, apart
/// from those of its Pinned branch and those a container still runs on. The Daemon then forgets every ended Job and
/// ended Service of the Worktree. Its Logical resources are kept and named. Running it again, or in a Worktree Procly
/// never saw, releases nothing. It then applies the Retention policy of the Isolated containers of the Worktrees
/// already removed.
///
/// With `dry_run` it answers with the report the same release would give and changes nothing, so it forgets no Job or
/// Service, and names the Retention policies it would apply without applying them.
pub(super) async fn run(
    registry: Arc<Registry>,
    path: PathBuf,
    dry_run: bool,
    peer: Option<i32>,
) -> anyhow::Result<ReleaseReport> {
    // Resolving the Topology and the branch runs git, and naming the requester runs ps.
    let (topology, branch, who) = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        let topology = topology_of(&path)?;
        // The Main worktree keeps Leases of its own whatever its branch, it never holds those of a Pinned branch.
        let branch = if topology.worktree.main {
            None
        } else {
            topology::branch(&topology.worktree.path)?
        };
        Ok((topology, branch, requester::describe(peer, &path)))
    })
    .await
    .context("the request could not be read")??;
    let worktree = topology.worktree.path.clone();
    if dry_run {
        eprintln!("worktree release dry run of {} requested by {who}", worktree.display());
        // It reads the disk, and listing the Worktrees of the removed Isolated containers runs git.
        return tokio::task::spawn_blocking(move || dry_run_of(&registry, &topology, branch.as_deref()))
            .await
            .context("the request could not be read")?;
    }
    eprintln!("worktree release of {} requested by {who}", worktree.display());

    // Before anything is stopped, so that a Restart under way starts nothing the release would miss, and a Restart
    // after it has no environment to start with.
    registry.held.release(&worktree).await;
    let services = registry.services.stop(&worktree, None);
    let jobs: Vec<_> = registry
        .jobs
        .running_in(&worktree)
        .into_iter()
        .map(|(id, job)| (id, tokio::spawn(async move { job.stop().await })))
        .collect();
    let (compose_services, isolated) = {
        let (registry, topology) = (Arc::clone(&registry), topology.clone());
        // Whether the other Worktrees still exist is read from the disk, and the record is written to it.
        tokio::task::spawn_blocking(move || {
            let composes = &registry.composes;
            let (repository, worktree) = (&topology.git_common_dir, &topology.worktree.path);
            (
                composes.release(repository, worktree, None),
                composes.isolated_released(repository, worktree, None),
            )
        })
        .await
        .context("the containers could not be released")?
    };
    // Like `down`, the containers of other Worktrees removed without it are stopped too, but are not this one's to tell.
    let isolated: Vec<String> = registry
        .composes
        .stop_isolated(isolated)
        .await?
        .into_iter()
        .filter(|(_, _, owner)| owner.as_deref() == Some(worktree.as_path()))
        .map(|(_, name, _)| name)
        .collect();
    // Once the containers of removed Worktrees are stopped, which a policy never deletes while they run.
    let retention = retention::sweep(&registry).await;
    let mut report = ReleaseReport {
        worktree: worktree.clone(),
        compose_services,
        retention,
        ..ReleaseReport::default()
    };
    for (name, service) in services {
        service.ended().await;
        report.services.push(name);
    }
    report.services.extend(isolated);
    for (id, stopping) in jobs {
        let _ = stopping.await;
        report.jobs.push(id);
    }
    // Everything that ran is stopped, so what the Daemon still holds of the Worktree is its ended Jobs and Services.
    registry.jobs.remove_ended_in(&worktree);
    registry.services.remove_ended_in(&worktree);

    // Once the Services are gone, so that only a container still running keeps a Lease of the Worktree.
    let (leases, kept_leases, logical_resources, warning) = {
        let registry = Arc::clone(&registry);
        tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
            let running = registry.running();
            let (leases, kept) = registry.leases.release_worktree(
                &topology.git_common_dir,
                &topology.worktree.path,
                branch.as_deref(),
                &running,
            )?;
            // The aliases of the Leases just released go with them.
            let warning = registry.portless.prune(&registry.leases);
            // The Leases are released whatever the configuration says, a broken one only hides the resources.
            let resources = logical_resources(&registry, &topology).unwrap_or_else(|error| {
                eprintln!(
                    "cannot name the Logical resources of {}, they are kept: {error:#}",
                    topology.worktree.path.display()
                );
                Vec::new()
            });
            Ok((leases, kept, resources, warning))
        })
        .await
        .context("the leases could not be released")??
    };
    report.leases = leases;
    report.kept_leases = kept_leases;
    report.logical_resources = logical_resources;
    report.warnings.extend(warning);
    log(&report);
    Ok(report)
}

/// The report a release of the Worktree of `topology` would give, computed from what the Daemon holds and the disk
/// without stopping, letting go of or writing anything. Blocking: it reads the disk.
fn dry_run_of(registry: &Registry, topology: &Topology, branch: Option<&str>) -> anyhow::Result<ReleaseReport> {
    let (repository, worktree) = (&topology.git_common_dir, &topology.worktree.path);
    let mut report = ReleaseReport {
        worktree: worktree.clone(),
        services: registry.services.would_stop(worktree),
        jobs: registry
            .jobs
            .running_in(worktree)
            .into_iter()
            .map(|(id, _)| id)
            .collect(),
        compose_services: registry.composes.would_release(repository, worktree, None),
        ..ReleaseReport::default()
    };
    // The Isolated containers it stops count as Services of the report, those of other removed Worktrees are not its own.
    let stopped_isolated = registry
        .composes
        .would_stop_isolated(registry.composes.isolated_released(repository, worktree, None));
    let is_stopped = |path: &Path, service: &str| {
        stopped_isolated
            .iter()
            .any(|(_, name, owner)| name == service && owner.as_deref() == Some(path))
    };
    // What runs when the release applies the Retention policies, once it has stopped the Isolated containers.
    let running_at_sweep = registry
        .running()
        .into_iter()
        .filter(|(path, service)| !is_stopped(path, service))
        .collect();
    // What still runs once the release is done: the Services of the Worktree and the Isolated containers are gone, so
    // that only a container left to its Grace period keeps a Lease of the Worktree.
    let mut running = registry.running();
    running.retain(|(path, _)| path != worktree);
    running.extend(
        registry
            .composes
            .running()
            .into_iter()
            .filter(|(path, service)| !is_stopped(path, service)),
    );
    report.services.extend(
        stopped_isolated
            .into_iter()
            .filter(|(_, _, owner)| owner.as_deref() == Some(worktree.as_path()))
            .map(|(_, name, _)| name),
    );
    let (leases, kept_leases) = registry
        .leases
        .would_release_worktree(repository, worktree, branch, &running)?;
    report.leases = leases;
    report.kept_leases = kept_leases;
    // A record that cannot be read leaves the policies out of the report and is only logged, as the release does.
    match retention::would_apply(registry, running_at_sweep) {
        Ok(applied) => report.retention = applied,
        Err(error) => eprintln!("cannot tell the retention policies a release would apply: {error:#}"),
    }
    // A configuration that cannot be read only hides the resources, the Leases are released whatever it says.
    match logical_resources(registry, topology) {
        Ok(resources) => report.logical_resources = resources,
        Err(error) => report.warnings.push(format!(
            "cannot name the Logical resources of {}, they are kept: {error:#}",
            worktree.display()
        )),
    }
    Ok(report)
}

/// The Logical resources the configuration gives the Worktree, which a Worktree release keeps: the named ones of each
/// Logical Compose service, and the numbered ones that hold an index. Blocking, it reads the configuration, the
/// Drivers and the indexes.
fn logical_resources(registry: &Registry, topology: &Topology) -> anyhow::Result<Vec<KeptResource>> {
    let worktree = &topology.worktree;
    let config = read_config(&worktree.path)?;
    let mut kept = Vec::new();
    for (service, declared) in &config.services {
        let Some(driver) = &declared.driver else {
            continue;
        };
        // A Shared Service holds the resources of the Main worktree, which are no Worktree's own.
        if declared.compose.is_none() || config.isolation(declared) != IsolationMode::Logical {
            continue;
        }
        let driver = Driver::load(driver, &worktree.path, driver::user_dir().as_deref())?;
        let mut keep = |resource: &str, name: String| {
            kept.push(KeptResource {
                service: service.clone(),
                driver: driver.name.clone(),
                resource: resource.to_owned(),
                name,
            });
        };
        if driver.index_capacity().is_some() {
            // An index is handed out before its resource is created, a resource without one was never created.
            let indexes = registry
                .indexes
                .held(&topology.git_common_dir, &worktree.path, service)?;
            for (resource, index) in indexes {
                keep(&resource, index.to_string());
            }
        } else {
            for (resource, declared) in &declared.resources {
                if let Some(name) = &declared.name {
                    keep(resource, driver.resource_name(name, &worktree.slug, worktree.main)?);
                }
            }
        }
    }
    Ok(kept)
}

/// Logs what the release did, after the line naming who asked for it.
fn log(report: &ReleaseReport) {
    let worktree = report.worktree.display();
    for service in &report.services {
        eprintln!("worktree release of {worktree} stopped service {service}");
    }
    for job in &report.jobs {
        eprintln!("worktree release of {worktree} stopped job {job}");
    }
    for released in &report.compose_services {
        eprintln!("worktree release of {worktree} let go of {}", released.service);
    }
    for lease in &report.leases {
        eprintln!(
            "worktree release of {worktree} released the lease of {}.{} on port {}",
            lease.service, lease.port_name, lease.port
        );
    }
}
