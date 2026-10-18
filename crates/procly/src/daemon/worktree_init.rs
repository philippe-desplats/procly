//! `procly worktree init`: copies the listed untracked files from the Main worktree into a Worktree and runs its
//! setup commands once (ADR 0011).
//!
//! The commands that succeeded are recorded in the git directory private to the Worktree, so the record goes away
//! with the Worktree and a Worktree recreated at the same path starts afresh.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use serde::{Deserialize, Serialize};

use super::Registry;
use super::job::{Fingerprint, JobSpec};
use crate::protocol::{InitReport, JobEnd, Response};
use crate::topology::{self, Topology};

/// How long a setup command may run before its whole process tree is killed, long enough for a cold install.
const SETUP_TIMEOUT: Duration = Duration::from_mins(30);
/// The record of the setup commands that succeeded, in the git directory of the Worktree.
const RECORD_FILE: &str = "procly-worktree-init.json";

#[derive(Debug, thiserror::Error)]
enum InitError {
    #[error("cannot copy {from} to {to}")]
    Copy {
        from: PathBuf,
        to: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cannot read {path}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("the Repository holding {worktree} has no Main worktree to copy from")]
    NoMainWorktree { worktree: PathBuf },
    #[error("cannot write {path}")]
    WriteRecord {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{path} is not valid, remove it to run every setup command again")]
    InvalidRecord {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
}

/// The setup commands that succeeded in a Worktree, by their text.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Record {
    setup: Vec<String>,
}

/// Where the Worktree stands once its files are copied, before any setup command runs.
struct Prepared {
    report: InitReport,
    setup: Vec<String>,
    record_path: PathBuf,
    record: Record,
}

/// Prepares the Worktree holding `path`: copies the listed files, then runs the setup commands it has not run, each
/// as a Job so that its whole process tree is killed on timeout or when the Daemon stops. Stops at the first one that
/// fails.
pub(super) async fn run(
    registry: Arc<Registry>,
    path: PathBuf,
    env: Vec<(String, String)>,
) -> anyhow::Result<Response> {
    // Resolving the Topology runs git.
    let topology = tokio::task::spawn_blocking(move || locate(&path))
        .await
        .context("the worktree could not be located")??;
    let lock = Arc::clone(
        registry
            .initializing
            .lock()
            .expect("the init lock table mutex is never poisoned, no code panics while holding it")
            .entry(topology.worktree.path.clone())
            .or_default(),
    );
    // Held to the end, so that a second init reads the record this one writes instead of racing it.
    let _initializing = lock.lock().await;
    // Reading the configuration and copying files are blocking.
    let prepared_topology = topology.clone();
    let Prepared {
        mut report,
        setup,
        record_path,
        mut record,
    } = tokio::task::spawn_blocking(move || prepare(&prepared_topology))
        .await
        .context("the worktree could not be prepared")??;
    for command in setup {
        if record.setup.contains(&command) {
            continue;
        }
        let spec = JobSpec {
            fingerprint: Fingerprint {
                topology: Some(topology.clone()),
                cwd: topology.worktree.path.clone(),
                command: command.clone(),
            },
            env: env.clone(),
            timeout: SETUP_TIMEOUT,
        };
        let jobs = Arc::clone(&registry);
        // Opening a terminal and forking are blocking system calls.
        let (id, _) = tokio::task::spawn_blocking(move || jobs.jobs.start(&spec))
            .await
            .context("the setup command could not be started")?
            .with_context(|| format!("cannot run the setup command {command:?}"))?;
        let job = registry
            .jobs
            .get(&id)
            .context("the setup command vanished from the job table")?;
        let end = job.ended().await;
        if end != (JobEnd::Exited { code: 0 }) {
            return Ok(Response::SetupFailed {
                report,
                command,
                end,
                output: job.output(),
            });
        }
        record.setup.push(command.clone());
        let (path, saved) = (record_path.clone(), serde_json::to_string(&record)?);
        tokio::task::spawn_blocking(move || write_record(&path, &saved))
            .await
            .context("the record could not be written")??;
        report.ran.push(command);
    }
    Ok(Response::WorktreeInit(report))
}

/// The Topology of the Worktree holding `path`.
fn locate(path: &Path) -> anyhow::Result<Topology> {
    let path = topology::canonicalize(path)?;
    topology::resolve(&path)?.with_context(|| {
        format!(
            "{} is not in a git repository, and Worktrees belong to a Repository",
            path.display()
        )
    })
}

fn prepare(topology: &Topology) -> anyhow::Result<Prepared> {
    let worktree = topology.worktree.path.clone();
    let config = super::read_config(&worktree)?;
    let main = topology::worktrees(&topology.git_common_dir)?
        .into_iter()
        .find(|candidate| candidate.main)
        .ok_or_else(|| InitError::NoMainWorktree {
            worktree: worktree.clone(),
        })?;
    let mut report = InitReport::default();
    // The Main worktree is where the files come from, it has nothing to copy onto itself.
    if main.path != worktree {
        let Copied { copied, missing } = copy_listed(&main.path, &worktree, &config.worktree.copy)?;
        report.copied = copied;
        report.missing = missing;
    }
    let record_path = topology::git_dir(&worktree)?.join(RECORD_FILE);
    let record = read_record(&record_path)?;
    Ok(Prepared {
        report,
        setup: config.worktree.setup,
        record_path,
        record,
    })
}

/// What copying changed in the Worktree, by the path as listed in the configuration.
#[derive(Debug, Default)]
struct Copied {
    copied: Vec<String>,
    /// Listed paths the Main worktree does not hold.
    missing: Vec<String>,
}

/// Copies each listed path from `main` into `worktree` unless the Worktree already holds it.
fn copy_listed(main: &Path, worktree: &Path, listed: &[String]) -> Result<Copied, InitError> {
    let mut outcome = Copied::default();
    for entry in listed {
        let source = main.join(entry);
        if source.symlink_metadata().is_err() {
            outcome.missing.push(entry.clone());
        } else if copy_entry(&source, &worktree.join(entry))? {
            outcome.copied.push(entry.clone());
        }
    }
    Ok(outcome)
}

/// The record at `path`, empty when the Worktree has not run any setup command.
fn read_record(path: &Path) -> Result<Record, InitError> {
    match fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).map_err(|source| InitError::InvalidRecord {
            path: path.to_owned(),
            source,
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Record::default()),
        Err(source) => Err(InitError::Read {
            path: path.to_owned(),
            source,
        }),
    }
}

/// Replaces the record at `path` in one step, so that a crash never leaves half of it.
fn write_record(path: &Path, text: &str) -> Result<(), InitError> {
    let write_error = |source| InitError::WriteRecord {
        path: path.to_owned(),
        source,
    };
    let partial = path.with_extension("json.partial");
    fs::write(&partial, text).map_err(write_error)?;
    fs::rename(&partial, path).map_err(write_error)
}

/// Copies `source` to `destination` wherever the destination is absent, directories file by file. Returns whether
/// anything was created.
fn copy_entry(source: &Path, destination: &Path) -> Result<bool, InitError> {
    let copy_error = |source_error| InitError::Copy {
        from: source.to_owned(),
        to: destination.to_owned(),
        source: source_error,
    };
    let kind = source.symlink_metadata().map_err(copy_error)?.file_type();
    let absent = destination.symlink_metadata().is_err();
    if kind.is_dir() {
        fs::create_dir_all(destination).map_err(copy_error)?;
        let entries = fs::read_dir(source).map_err(|source_error| InitError::Read {
            path: source.to_owned(),
            source: source_error,
        })?;
        let mut copied = absent;
        for child in entries {
            let child = child.map_err(|source_error| InitError::Read {
                path: source.to_owned(),
                source: source_error,
            })?;
            copied |= copy_entry(&child.path(), &destination.join(child.file_name()))?;
        }
        return Ok(copied);
    }
    if !absent {
        return Ok(false);
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(copy_error)?;
    }
    if kind.is_symlink() {
        let target = fs::read_link(source).map_err(copy_error)?;
        std::os::unix::fs::symlink(target, destination).map_err(copy_error)?;
    } else {
        fs::copy(source, destination).map_err(copy_error)?;
    }
    Ok(true)
}
