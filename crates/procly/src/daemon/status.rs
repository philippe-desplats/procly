//! The status report: Repositories and their Worktrees, with the Services and the Jobs started in each.

use std::path::{Path, PathBuf};

use super::compose::Composes;
use super::foreign;
use super::job::Jobs;
use super::service::Services;
use crate::protocol::{JobStatus, RepositoryStatus, ServiceStatus, StatusReport, WorktreeStatus};
use crate::topology::{self, Topology, Worktree};

/// The Worktree holding `cwd`, or with `all` every Worktree of every Repository the Daemon ran a Service or a Job
/// in, plus the Repository holding `cwd`. Outside any Repository, the Jobs started in `cwd` stand in for the
/// Worktree. Each Worktree also lists its Foreign processes, which every process group and container Procly runs
/// is not.
pub(super) fn report(
    jobs: &Jobs,
    services: &Services,
    composes: &Composes,
    cwd: &Path,
    all: bool,
) -> anyhow::Result<StatusReport> {
    let cwd = topology::canonicalize(cwd)?;
    let here = topology::resolve(&cwd)?;
    let owned_groups = services
        .process_groups()
        .into_iter()
        .chain(jobs.process_groups())
        .collect();
    let statuses = jobs.statuses();
    let services = services.statuses();
    let containers = composes.isolated();
    // Every Worktree the Daemon ran something in.
    let known: Vec<&Topology> = services
        .iter()
        .map(|(topology, _)| topology)
        .chain(statuses.iter().filter_map(|(topology, _)| topology.as_ref()))
        .collect();

    let mut git_common_dirs: Vec<&PathBuf> = here.iter().map(|here| &here.git_common_dir).collect();
    if all {
        git_common_dirs.extend(known.iter().map(|topology| &topology.git_common_dir));
    }
    git_common_dirs.sort();
    git_common_dirs.dedup();

    let mut repositories: Vec<RepositoryStatus> = git_common_dirs
        .into_iter()
        .map(|git_common_dir| {
            let worktrees = match (&here, all) {
                (Some(here), false) => vec![here.worktree.clone()],
                _ => worktrees(git_common_dir, &known),
            };
            RepositoryStatus {
                git_common_dir: git_common_dir.clone(),
                worktrees: worktrees
                    .into_iter()
                    .map(|worktree| WorktreeStatus {
                        services: services_of(&services, &worktree.path),
                        jobs: jobs_where(&statuses, |topology| {
                            topology.is_some_and(|topology| topology.worktree.path == worktree.path)
                        }),
                        containers: containers
                            .iter()
                            .filter(|(root, _)| *root == worktree.path)
                            .map(|(_, container)| container.clone())
                            .collect(),
                        worktree,
                        foreign: Vec::new(),
                    })
                    .collect(),
            }
        })
        .collect();
    let reported: Vec<PathBuf> = repositories
        .iter()
        .flat_map(|repository| repository.worktrees.iter().map(|status| status.worktree.path.clone()))
        .collect();
    let mut found = foreign::in_worktrees(&reported, &owned_groups, &composes.owned()).into_iter();
    for status in repositories
        .iter_mut()
        .flat_map(|repository| repository.worktrees.iter_mut())
    {
        status.foreign = found.next().unwrap_or_default();
    }
    let jobs_outside_repositories = if all {
        jobs_where(&statuses, |topology| topology.is_none())
    } else if here.is_none() {
        let mut jobs = jobs_where(&statuses, |topology| topology.is_none());
        jobs.retain(|job| job.cwd == cwd);
        jobs
    } else {
        Vec::new()
    };
    Ok(StatusReport {
        repositories,
        jobs_outside_repositories,
    })
}

/// The Worktrees git lists for a Repository, followed by the ones its Services and Jobs ran in that git no longer
/// lists.
fn worktrees(git_common_dir: &Path, known: &[&Topology]) -> Vec<Worktree> {
    // A Repository deleted since its Jobs ran is still reported, through the Worktrees they recorded.
    let mut worktrees = topology::worktrees(git_common_dir).unwrap_or_default();
    for topology in known {
        if topology.git_common_dir == git_common_dir
            && !worktrees.iter().any(|worktree| worktree.path == topology.worktree.path)
        {
            worktrees.push(topology.worktree.clone());
        }
    }
    worktrees
}

fn services_of(services: &[(Topology, ServiceStatus)], worktree: &Path) -> Vec<ServiceStatus> {
    services
        .iter()
        .filter(|(topology, _)| topology.worktree.path == worktree)
        .map(|(_, service)| service.clone())
        .collect()
}

fn jobs_where(
    statuses: &[(Option<Topology>, JobStatus)],
    belongs: impl Fn(Option<&Topology>) -> bool,
) -> Vec<JobStatus> {
    statuses
        .iter()
        .filter(|(topology, _)| belongs(topology.as_ref()))
        .map(|(_, job)| job.clone())
        .collect()
}
