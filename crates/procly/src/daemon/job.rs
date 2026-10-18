//! Jobs: ad hoc commands run in a PTY, in a process group of their own, with a timeout.

use std::collections::HashMap;
use std::hash::BuildHasher as _;
use std::io::{self, Read};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use nix::errno::Errno;
use nix::sys::signal::{Signal, killpg};
use nix::sys::wait::{WaitStatus, waitpid};
use nix::unistd::Pid;
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use tokio::sync::{oneshot, watch};
use tokio::task::JoinHandle;

use crate::output::OutputBuffer;
use crate::protocol::JobEnd;

/// The recent output kept per Job.
const OUTPUT_CAPACITY: usize = 1024 * 1024;
/// How long a timed-out Job's tree gets between `SIGTERM` and `SIGKILL`.
const STOP_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the output of an ended Job is still read, when a descendant that left its group keeps its terminal open.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(1);
/// How many times a group is sent `SIGKILL` before giving up on members that keep appearing.
const KILL_ATTEMPTS: u32 = 100;
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
    pub(crate) command: String,
    pub(crate) cwd: PathBuf,
    pub(crate) env: Vec<(String, String)>,
    pub(crate) timeout: Duration,
}

/// Every Job the Daemon knows about, running or ended.
#[derive(Debug, Default)]
pub(crate) struct Jobs {
    table: Mutex<HashMap<String, Arc<Job>>>,
}

#[derive(Debug)]
pub(crate) struct Job {
    /// The PTY child leads its own session, so its pid is also the id of the process group holding its tree.
    pid: Pid,
    output: Arc<Mutex<OutputBuffer>>,
    end: watch::Sender<Option<JobEnd>>,
}

impl Jobs {
    /// Spawns the Job and returns its identifier, without waiting for it. Must run inside the Tokio runtime.
    pub(crate) fn start(&self, spec: &JobSpec) -> Result<String, JobError> {
        let pair = native_pty_system().openpty(PTY_SIZE).map_err(JobError::OpenPty)?;
        // Taken before the spawn, so that no error path is left between spawning and supervising the child.
        let reader = pair.master.try_clone_reader().map_err(JobError::OpenPty)?;
        let mut command = CommandBuilder::new("/bin/sh");
        command.args(["-c", &spec.command]);
        command.cwd(&spec.cwd);
        command.env_clear();
        for (name, value) in &spec.env {
            command.env(name, value);
        }
        let mut child = pair.slave.spawn_command(command).map_err(|source| JobError::Spawn {
            command: spec.command.clone(),
            cwd: spec.cwd.clone(),
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

        let output = Arc::new(Mutex::new(OutputBuffer::new(OUTPUT_CAPACITY)));
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
        Ok(self.insert(job))
    }

    pub(crate) fn get(&self, id: &str) -> Option<Arc<Job>> {
        self.table().get(id).cloned()
    }

    /// Kills the tree of every running Job, for a Daemon about to exit.
    pub(crate) fn kill_all(&self) {
        for job in self.table().values() {
            if job.end.borrow().is_none() {
                kill_group(job.pid);
            }
        }
    }

    fn insert(&self, job: Arc<Job>) -> String {
        let mut table = self.table();
        loop {
            let id = new_job_id();
            if !table.contains_key(&id) {
                table.insert(id.clone(), job);
                return id;
            }
        }
    }

    fn table(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<Job>>> {
        self.table
            .lock()
            .expect("the job table mutex is never poisoned, no code panics while holding it")
    }
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
        lock_output(&self.output).text()
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

/// Sends `SIGKILL` to the group until it is empty, so that a member forked while the first signal was in flight
/// goes too. The group keeps its id reserved while any member lives, so this never reaches an unrelated process.
fn kill_group(pgid: Pid) {
    for _ in 0..KILL_ATTEMPTS {
        match killpg(pgid, Signal::SIGKILL) {
            Ok(()) => std::thread::sleep(Duration::from_millis(10)),
            Err(Errno::ESRCH) => return,
            Err(error) => {
                eprintln!("cannot kill process group {pgid}: {error}");
                return;
            }
        }
    }
    eprintln!("process group {pgid} still has members after {KILL_ATTEMPTS} SIGKILL");
}

fn signal_group(pgid: Pid, signal: Signal) {
    match killpg(pgid, signal) {
        Ok(()) | Err(Errno::ESRCH) => {}
        Err(error) => eprintln!("cannot send {signal} to process group {pgid}: {error}"),
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
            Ok(read) => lock_output(output).push(&chunk[..read]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            // Linux reports EIO once the last slave handle closes, which is the end of the output.
            Err(_) => return,
        }
    }
}

fn lock_output(output: &Mutex<OutputBuffer>) -> std::sync::MutexGuard<'_, OutputBuffer> {
    output
        .lock()
        .expect("an output buffer mutex is never poisoned, no code panics while holding it")
}

/// A short random identifier, easy for an agent to copy.
fn new_job_id() -> String {
    let random = std::hash::RandomState::new().hash_one(SystemTime::now());
    format!("{:08x}", random & 0xffff_ffff)
}
