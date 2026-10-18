//! Jobs: ad hoc commands run in a PTY, in a process group of their own, with a timeout.

use std::collections::HashMap;
use std::hash::BuildHasher as _;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use nix::errno::Errno;
use nix::sys::signal::Signal;
use nix::sys::wait::{WaitStatus, waitpid};
use nix::unistd::Pid;
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use tokio::sync::{oneshot, watch};
use tokio::task::JoinHandle;

use super::process_group::{DRAIN_TIMEOUT, STOP_TIMEOUT, kill_group, signal_group};
use crate::output::{self, OutputBuffer};
use crate::protocol::{JobEnd, JobState, JobStatus, StartOutcome};
use crate::topology::Topology;

const PTY_SIZE: PtySize = PtySize {
    rows: 24,
    cols: 120,
    pixel_width: 0,
    pixel_height: 0,
};

#[derive(Debug, thiserror::Error)]
pub(crate) enum JobError {
    #[error("cannot open a terminal for the job")]
    OpenPty(#[source] anyhow::Error),
    #[error("cannot start {command:?} in {cwd}")]
    Spawn {
        command: String,
        cwd: PathBuf,
        #[source]
        source: anyhow::Error,
    },
    #[error("the job started without a process id")]
    NoPid,
}

#[derive(Debug)]
pub(crate) struct JobSpec {
    pub(crate) fingerprint: Fingerprint,
    pub(crate) env: Vec<(String, String)>,
    pub(crate) timeout: Duration,
}

/// The identity of a Job: its Worktree, working directory and command.
#[derive(Debug)]
pub(crate) struct Fingerprint {
    /// `None` outside any Repository. The rest of the Topology is kept to report the Job.
    pub(crate) topology: Option<Topology>,
    /// Canonicalized, so that 2 spellings of one directory give one Fingerprint.
    pub(crate) cwd: PathBuf,
    pub(crate) command: String,
}

impl Fingerprint {
    fn matches(&self, other: &Self) -> bool {
        self.worktree() == other.worktree() && self.cwd == other.cwd && self.command == other.command
    }

    fn worktree(&self) -> Option<&Path> {
        self.topology.as_ref().map(|topology| topology.worktree.path.as_path())
    }
}

/// Every Job the Daemon knows about, running or ended.
#[derive(Debug, Default)]
pub(crate) struct Jobs {
    table: Mutex<HashMap<String, Arc<Job>>>,
    /// Held from the Fingerprint lookup to the insert, so that 2 concurrent requests never both start the Job,
    /// while the table itself stays free for requests that only read it.
    starting: Mutex<()>,
}

#[derive(Debug)]
pub(crate) struct Job {
    fingerprint: Fingerprint,
    started_at: Instant,
    /// The PTY child leads its own session, so its pid is also the id of the process group holding its tree.
    pid: Pid,
    output: Arc<Mutex<OutputBuffer>>,
    end: watch::Sender<Option<JobEnd>>,
}

impl Jobs {
    /// Returns the running Job with the Fingerprint of `spec`, or spawns a new one without waiting for it.
    /// Must run inside the Tokio runtime.
    pub(crate) fn start(&self, spec: &JobSpec) -> Result<(String, StartOutcome), JobError> {
        // Held through the spawn, which takes milliseconds on a blocking thread and never spans an await.
        let _starting = self
            .starting
            .lock()
            .expect("the start mutex is never poisoned, no code panics while holding it");
        let running = self
            .table()
            .iter()
            .find(|(_, job)| job.fingerprint.matches(&spec.fingerprint) && job.end.borrow().is_none())
            .map(|(id, _)| id.clone());
        if let Some(id) = running {
            return Ok((id, StartOutcome::Reused));
        }
        let job = spawn(spec)?;
        let mut table = self.table();
        loop {
            let id = new_job_id();
            if !table.contains_key(&id) {
                table.insert(id.clone(), job);
                return Ok((id, StartOutcome::Started));
            }
        }
    }

    pub(crate) fn get(&self, id: &str) -> Option<Arc<Job>> {
        self.table().get(id).cloned()
    }

    /// Every Job with its Topology, in the order they started.
    pub(crate) fn statuses(&self) -> Vec<(Option<Topology>, JobStatus)> {
        let mut jobs: Vec<_> = self
            .table()
            .iter()
            .map(|(id, job)| (id.clone(), Arc::clone(job)))
            .collect();
        jobs.sort_by_key(|(_, job)| job.started_at);
        jobs.into_iter()
            .map(|(id, job)| {
                let state = match *job.end.borrow() {
                    Some(end) => JobState::Ended { end },
                    None => JobState::Running,
                };
                let status = JobStatus {
                    id,
                    command: job.fingerprint.command.clone(),
                    cwd: job.fingerprint.cwd.clone(),
                    state,
                };
                (job.fingerprint.topology.clone(), status)
            })
            .collect()
    }

    /// The process group of every running Job.
    pub(crate) fn process_groups(&self) -> Vec<i32> {
        self.table()
            .values()
            .filter(|job| job.end.borrow().is_none())
            .map(|job| job.pid.as_raw())
            .collect()
    }

    /// Kills the tree of every running Job, for a Daemon about to exit.
    pub(crate) fn kill_all(&self) {
        for job in self.table().values() {
            if job.end.borrow().is_none() {
                kill_group(job.pid);
            }
        }
    }

    fn table(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<Job>>> {
        self.table
            .lock()
            .expect("the job table mutex is never poisoned, no code panics while holding it")
    }
}

/// Spawns the Job in a PTY and starts supervising it.
fn spawn(spec: &JobSpec) -> Result<Arc<Job>, JobError> {
    let Fingerprint {
        command: shell_command,
        cwd,
        ..
    } = &spec.fingerprint;
    let pair = native_pty_system().openpty(PTY_SIZE).map_err(JobError::OpenPty)?;
    // Taken before the spawn, so that no error path is left between spawning and supervising the child.
    let reader = pair.master.try_clone_reader().map_err(JobError::OpenPty)?;
    let mut command = CommandBuilder::new("/bin/sh");
    command.args(["-c", shell_command]);
    command.cwd(cwd);
    command.env_clear();
    for (name, value) in &spec.env {
        command.env(name, value);
    }
    let mut child = pair.slave.spawn_command(command).map_err(|source| JobError::Spawn {
        command: shell_command.clone(),
        cwd: cwd.clone(),
        source,
    })?;
    // The reader below only reaches end of file once every slave handle is closed.
    drop(pair.slave);

    let pid = child
        .process_id()
        .and_then(|pid| i32::try_from(pid).ok())
        .map(Pid::from_raw);
    let Some(pid) = pid else {
        // Unreachable on Unix, where every spawned child has a pid, but never leave a child unreaped.
        let _ = child.kill();
        let _ = child.wait();
        return Err(JobError::NoPid);
    };

    let output = Arc::new(Mutex::new(OutputBuffer::new(output::CAPACITY)));
    let (reader_done_tx, reader_done) = oneshot::channel();
    let pump_output = Arc::clone(&output);
    tokio::task::spawn_blocking(move || {
        pump(reader, &pump_output);
        let _ = reader_done_tx.send(());
    });
    let waiter = tokio::task::spawn_blocking(move || {
        let status = wait_for_exit(pid);
        // Reaped by `wait_for_exit`, the handle has nothing left to release.
        drop(child);
        status
    });

    let job = Arc::new(Job {
        fingerprint: Fingerprint {
            topology: spec.fingerprint.topology.clone(),
            cwd: cwd.clone(),
            command: shell_command.clone(),
        },
        started_at: Instant::now(),
        pid,
        output,
        end: watch::Sender::new(None),
    });
    tokio::spawn(supervise(
        Arc::clone(&job),
        pair.master,
        waiter,
        reader_done,
        spec.timeout,
    ));
    Ok(job)
}

impl Job {
    /// Waits until the Job has ended and its output has been read.
    pub(crate) async fn ended(&self) -> JobEnd {
        let mut end = self.end.subscribe();
        let end = end
            .wait_for(Option::is_some)
            .await
            .expect("the job holds its own end sender, so the channel never closes");
        end.expect("wait_for returned on a Some")
    }

    pub(crate) fn output(&self) -> String {
        output::lock(&self.output).text()
    }
}

async fn supervise(
    job: Arc<Job>,
    master: Box<dyn MasterPty + Send>,
    mut waiter: JoinHandle<WaitStatus>,
    reader_done: oneshot::Receiver<()>,
    timeout: Duration,
) {
    let end = if let Ok(status) = tokio::time::timeout(timeout, &mut waiter).await {
        end_of(status.expect("the exit waiter panics only when the daemon lost track of its own child"))
    } else {
        kill_tree(job.pid, waiter).await;
        JobEnd::TimedOut {
            timeout_ms: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
        }
    };
    // A Job is bounded: members of its group still running once the leader has ended go with it.
    let pgid = job.pid;
    let _ = tokio::task::spawn_blocking(move || kill_group(pgid)).await;
    let _ = tokio::time::timeout(DRAIN_TIMEOUT, reader_done).await;
    drop(master);
    job.end.send_replace(Some(end));
}

/// Stops the leader of the process group: `SIGTERM` to the whole group, then `SIGKILL` after the stop timeout.
async fn kill_tree(pgid: Pid, mut waiter: JoinHandle<WaitStatus>) {
    signal_group(pgid, Signal::SIGTERM);
    if tokio::time::timeout(STOP_TIMEOUT, &mut waiter).await.is_err() {
        signal_group(pgid, Signal::SIGKILL);
        let _ = waiter.await;
    }
}

/// Reaps the Job's leader with `waitpid`, which tells an exit code from a signal, unlike `portable_pty::Child`.
fn wait_for_exit(pid: Pid) -> WaitStatus {
    loop {
        match waitpid(pid, None) {
            Ok(status @ (WaitStatus::Exited(..) | WaitStatus::Signaled(..))) => return status,
            Ok(_) | Err(Errno::EINTR) => {}
            Err(error) => panic!("cannot wait for job process {pid}, which only the daemon reaps: {error}"),
        }
    }
}

fn end_of(status: WaitStatus) -> JobEnd {
    match status {
        WaitStatus::Signaled(_, signal, _) => JobEnd::Signaled { signal: signal as i32 },
        WaitStatus::Exited(_, code) => JobEnd::Exited { code },
        other => unreachable!("wait_for_exit returns only exits and signals, got {other:?}"),
    }
}

fn pump(mut reader: Box<dyn Read + Send>, output: &Mutex<OutputBuffer>) {
    let mut chunk = [0; 8192];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => return,
            Ok(read) => output::lock(output).push(&chunk[..read]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            // Linux reports EIO once the last slave handle closes, which is the end of the output.
            Err(_) => return,
        }
    }
}

/// A short random identifier, easy for an agent to copy.
fn new_job_id() -> String {
    let random = std::hash::RandomState::new().hash_one(SystemTime::now());
    format!("{:08x}", random & 0xffff_ffff)
}
