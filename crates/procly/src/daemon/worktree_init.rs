//! `procly worktree init`: copies the listed untracked files from the Main worktree into a Worktree and runs its
//! setup commands once (ADR 0011).
//!
//! The commands that succeeded are recorded in the git directory private to the Worktree, so the record goes away
//! with the Worktree and a Worktree recreated at the same path starts afresh.

use std::fs::{self, DirBuilder, File, OpenOptions};
use std::hash::{DefaultHasher, Hash as _, Hasher as _};
use std::io::{self, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::UnboundedSender;
use tokio::time::MissedTickBehavior;

use super::Registry;
use super::atomic_file::{self, Mode};
use super::job::{Fingerprint, Job, JobSpec};
use super::store;
use crate::protocol::{InitReport, JobEnd, Response};
use crate::topology::{self, Topology};

/// The record of the setup commands that succeeded, in the git directory of the Worktree.
const RECORD_FILE: &str = "procly-worktree-init.json";
/// The version of the format of the record, a state document like the ones of the state directory (ADR 0012).
const RECORD_VERSION: u32 = 1;
/// The shortest interval between 2 progress reports, the elapsed time they print being in whole seconds.
const MIN_PROGRESS_INTERVAL: Duration = Duration::from_secs(1);

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
}

/// The setup commands that succeeded in a Worktree, by their text.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct Record {
    setup: Vec<String>,
}

/// Where the Worktree stands once its files are copied, before any setup command runs.
struct Prepared {
    report: InitReport,
    setup: Vec<String>,
    /// How long each setup command may run before its whole process tree is killed.
    setup_timeout: Duration,
    /// How often a setup command still running reports it.
    progress_interval: Duration,
    record_path: PathBuf,
    record: Record,
}

/// Prepares the Worktree holding `path`: copies the listed files, then runs the setup commands it has not run, each
/// as a Job so that its whole process tree is killed on timeout or when the Daemon stops. Stops at the first one that
/// fails. A command still running reports its elapsed time and its log through `progress`, once per
/// `settings.setup_progress_interval`.
///
/// The output of the commands is appended to a log in the state directory, one per Worktree and emptied by the next
/// `worktree init`. It is removed once every command succeeded, and kept when one failed, to tell why.
pub(super) async fn run(
    registry: Arc<Registry>,
    path: PathBuf,
    env: Vec<(String, String)>,
    progress: &UnboundedSender<Response>,
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
        setup_timeout,
        progress_interval,
        record_path,
        mut record,
    } = tokio::task::spawn_blocking(move || prepare(&prepared_topology))
        .await
        .context("the worktree could not be prepared")??;
    let mut log: Option<SetupLog> = None;
    for command in setup {
        if record.setup.contains(&command) {
            continue;
        }
        if log.is_none() {
            let (dir, worktree) = (registry.setup_logs.clone(), topology.worktree.path.clone());
            log = Some(
                tokio::task::spawn_blocking(move || SetupLog::create(&dir, &worktree))
                    .await
                    .context("the setup log could not be created")??,
            );
        }
        let log = log.as_ref().expect("the setup log was created above");
        let jobs = Arc::clone(&registry);
        let (topology, env, file, heading) = (topology.clone(), env.clone(), Arc::clone(&log.file), command.clone());
        // Opening a terminal and forking are blocking system calls, and so is sharing the log with the Job.
        let (id, _) = tokio::task::spawn_blocking(move || {
            // Best effort, the log is a courtesy to whoever waits.
            let _ = writeln!(&*file, "$ {heading}");
            let spec = JobSpec {
                fingerprint: Fingerprint {
                    cwd: topology.worktree.path.clone(),
                    topology: Some(topology),
                    command: heading,
                },
                env,
                timeout: setup_timeout,
                log: Some(file.try_clone().context("the setup log could not be shared")?),
            };
            anyhow::Ok(jobs.jobs.start(&spec)?)
        })
        .await
        .context("the setup command could not be started")?
        .with_context(|| format!("cannot run the setup command {command:?}"))?;
        let job = registry
            .jobs
            .get(&id)
            .context("the setup command vanished from the job table")?;
        let end = wait_reporting(&job, &command, &log.path, progress_interval, progress).await;
        if end != (JobEnd::Exited { code: 0 }) {
            return Ok(Response::SetupFailed {
                report,
                command,
                end,
                output: job.output(),
            });
        }
        record.setup.push(command.clone());
        let (path, saved) = (record_path.clone(), record.clone());
        tokio::task::spawn_blocking(move || store::save(&path, RECORD_VERSION, &saved))
            .await
            .context("the record could not be written")??;
        report.ran.push(command);
    }
    if let Some(log) = log {
        // Every command succeeded, nothing is left to tell.
        let _ = tokio::task::spawn_blocking(move || fs::remove_file(&log.path)).await;
    }
    Ok(Response::WorktreeInit(report))
}

/// The log the output of the setup commands of a Worktree is appended to.
struct SetupLog {
    path: PathBuf,
    /// Shared with the Jobs writing to it, so that the output of the commands follows each other.
    file: Arc<File>,
}

impl SetupLog {
    /// Creates the log of the Worktree at `worktree` in `dir`, readable by the user only, replacing the one a previous
    /// `worktree init` left. Blocking.
    fn create(dir: &Path, worktree: &Path) -> anyhow::Result<Self> {
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .with_context(|| format!("cannot create {}", dir.display()))?;
        // One name per Worktree, so that the logs of the Worktrees running their setup at once never mix.
        let mut hasher = DefaultHasher::new();
        worktree.hash(&mut hasher);
        let path = dir.join(format!("{:016x}.log", hasher.finish()));
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)
            .with_context(|| format!("cannot create the setup log {}", path.display()))?;
        Ok(Self {
            path,
            file: Arc::new(file),
        })
    }
}

/// Waits until `job` has ended, reporting through `progress` that the setup command is still running every `interval`.
async fn wait_reporting(
    job: &Job,
    command: &str,
    log: &Path,
    interval: Duration,
    progress: &UnboundedSender<Response>,
) -> JobEnd {
    let started = Instant::now();
    let mut ticks = tokio::time::interval_at(tokio::time::Instant::now() + interval, interval);
    // A tick missed because the loop was late is not made up for, so that reports never come in a burst.
    ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let ended = job.ended();
    tokio::pin!(ended);
    loop {
        tokio::select! {
            end = &mut ended => return end,
            _ = ticks.tick() => {
                // The client may be gone, the command runs on until it ends or times out.
                let _ = progress.send(Response::SetupProgress {
                    command: command.to_owned(),
                    elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                    log: log.to_owned(),
                });
            }
        }
    }
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
    let mut report = InitReport::default();
    // Only a copy needs the Main worktree, and a bare Repository has none.
    if !config.worktree.copy.is_empty() {
        let main = topology::worktrees(&topology.git_common_dir)?
            .into_iter()
            .find(|candidate| candidate.main)
            .ok_or_else(|| InitError::NoMainWorktree {
                worktree: worktree.clone(),
            })?;
        // The Main worktree is where the files come from, it has nothing to copy onto itself.
        if main.path != worktree {
            copy_listed(&main.path, &worktree, &config.worktree.copy, &mut report)?;
        }
    }
    let record_path = topology::git_dir(&worktree)?.join(RECORD_FILE);
    let record = store::load(&record_path, RECORD_VERSION)?.unwrap_or_default();
    Ok(Prepared {
        report,
        setup: config.worktree.setup,
        setup_timeout: config.settings.setup_timeout,
        progress_interval: config.settings.setup_progress_interval.max(MIN_PROGRESS_INTERVAL),
        record_path,
        record,
    })
}

/// Copies each listed path from `main` into `worktree` unless the Worktree already holds it, and notes in `report`
/// which ones it copied and which ones `main` lacks.
fn copy_listed(main: &Path, worktree: &Path, listed: &[String], report: &mut InitReport) -> Result<(), InitError> {
    for entry in listed {
        let source = main.join(entry);
        if exists(&source)? {
            if copy_entry(&source, &worktree.join(entry))? {
                report.copied.push(entry.clone());
            }
        } else {
            report.missing.push(entry.clone());
        }
    }
    Ok(())
}

/// Whether anything, a dangling symlink included, sits at `path`. Any other failure than absence is an error, so that
/// a permission problem never reads as a missing file.
fn exists(path: &Path) -> Result<bool, InitError> {
    match path.symlink_metadata() {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(InitError::Read {
            path: path.to_owned(),
            source,
        }),
    }
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
    let absent = !exists(destination)?;
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
        // Copied aside then renamed, so that an interrupted copy never leaves a truncated file that counts as present.
        let mode = Mode::Exactly(source.metadata().map_err(copy_error)?.permissions());
        atomic_file::replace_with(destination, mode, |file| {
            io::copy(&mut fs::File::open(source)?, file).map(|_| ())
        })
        .map_err(copy_error)?;
    }
    Ok(true)
}
