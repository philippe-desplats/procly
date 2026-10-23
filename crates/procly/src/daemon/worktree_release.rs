//! Worktree release: what a Worktree holds in Procly undone before it is removed, its data kept (ADR 0004).

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context as _;

use super::{Registry, read_config, requester, retention, topology_of};
use crate::config::IsolationMode;
use crate::driver::{self, Driver};
use crate::protocol::{KeptResource, ReleaseReport};
use crate::topology::{self, Topology};

/// Releases the Worktree holding `path`, logging who asked: stops its Services and Jobs with their whole trees, lets
/// go of its Compose services, then releases its Leases, apart from those of its Pinned branch and those a container
/// still runs on. Its Logical resources are kept and named. Running it again, or in a Worktree Procly never saw,
/// releases nothing. It then applies the Retention policy of the Isolated containers of the Worktrees already removed.
pub(super) async fn run(registry: Arc<Registry>, path: PathBuf, peer: Option<i32>) -> anyhow::Result<ReleaseReport> {
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
    eprintln!("worktree release of {} requested by {who}", worktree.display());

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
    retention::sweep(&registry).await;
    let mut report = ReleaseReport {
        worktree: worktree.clone(),
        compose_services,
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

    // Once the Services are gone, so that only a container still running keeps a Lease of the Worktree.
    let (leases, kept_leases, logical_resources) = {
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
            registry.portless.prune(&registry.leases);
            // The Leases are released whatever the configuration says, a broken one only hides the resources.
            let resources = logical_resources(&registry, &topology).unwrap_or_else(|error| {
                eprintln!(
                    "cannot name the Logical resources of {}, they are kept: {error:#}",
                    topology.worktree.path.display()
                );
                Vec::new()
            });
            Ok((leases, kept, resources))
        })
        .await
        .context("the leases could not be released")??
    };
    report.leases = leases;
    report.kept_leases = kept_leases;
    report.logical_resources = logical_resources;
    log(&report);
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
