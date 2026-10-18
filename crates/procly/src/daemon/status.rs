//! The status report: Repositories and their Worktrees, with the Jobs started in each.

use std::path::{Path, PathBuf};

use super::job::Jobs;
use crate::protocol::{JobStatus, RepositoryStatus, StatusReport, WorktreeStatus};
use crate::topology::{self, Topology, Worktree};

/// The Worktree holding `cwd`, or with `all` every Worktree of every Repository the Daemon ran a Job in, plus
/// the Repository holding `cwd`. Outside any Repository, the Jobs started in `cwd` stand in for the Worktree.
pub(super) fn report(jobs: &Jobs, cwd: &Path, all: bool) -> anyhow::Result<StatusReport> {
    let cwd = topology::canonicalize(cwd)?;
    let here = topology::resolve(&cwd)?;
    let statuses = jobs.statuses();

    let mut git_common_dirs: Vec<&PathBuf> = here.iter().map(|here| &here.git_common_dir).collect();
    if all {
        git_common_dirs.extend(
            statuses
                .iter()
                .filter_map(|(topology, _)| Some(&topology.as_ref()?.git_common_dir)),
        );
    }
    git_common_dirs.sort();
    git_common_dirs.dedup();

    let repositories = git_common_dirs
        .into_iter()
        .map(|git_common_dir| {
            let worktrees = match (&here, all) {
                (Some(here), false) => vec![here.worktree.clone()],
                _ => worktrees(git_common_dir, &statuses),
            };
            RepositoryStatus {
                git_common_dir: git_common_dir.clone(),
                worktrees: worktrees
                    .into_iter()
                    .map(|worktree| WorktreeStatus {
                        jobs: jobs_where(&statuses, |topology| {
                            topology.is_some_and(|topology| topology.worktree.path == worktree.path)
                        }),
                        worktree,
                    })
                    .collect(),
            }
        })
        .collect();
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

/// The Worktrees git lists for a Repository, followed by the ones its Jobs ran in that git no longer lists.
fn worktrees(git_common_dir: &Path, statuses: &[(Option<Topology>, JobStatus)]) -> Vec<Worktree> {
    // A Repository deleted since its Jobs ran is still reported, through the Worktrees they recorded.
    let mut worktrees = topology::worktrees(git_common_dir).unwrap_or_default();
    for (topology, _) in statuses {
        if let Some(topology) = topology
            .as_ref()
            .filter(|topology| topology.git_common_dir == git_common_dir)
            && !worktrees.iter().any(|worktree| worktree.path == topology.worktree.path)
        {
            worktrees.push(topology.worktree.clone());
        }
    }
    worktrees
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
