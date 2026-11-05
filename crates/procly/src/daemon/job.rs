//! Jobs: ad hoc commands run in a PTY, in a process group of their own, with a timeout.

use std::collections::HashMap;
use std::fs::File;
use std::hash::BuildHasher as _;
use std::io::{self, Read, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use tokio::sync::{oneshot, watch};
use tokio::task::JoinHandle;

use nix::sys::signal::Signal;

use super::process_group::{self, DRAIN_TIMEOUT, Pid, STOP_TIMEOUT, kill_group, signal_group};
use crate::output::{self, ENDED_JOBS_PER_WORKTREE, ENDED_JOBS_TOTAL, Follow, OutputBuffer};
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
    /// A file the output of the Job is also appended to, for a Job whose output outlives its buffer.
    pub(crate) log: Option<File>,
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

/// Every Job the Daemon knows about, running or ended, an ended one until a bound evicts it.
#[derive(Debug, Default)]
pub(crate) struct Jobs {
    table: Arc<Table>,
    /// Held from the Fingerprint lookup to the insert, so that 2 concurrent requests never both start the Job,
    /// while the table itself stays free for requests that only read it.
    starting: Mutex<()>,
}

/// The Jobs by identifier, shared with the task supervising each Job, which evicts when its Job ends.
#[derive(Debug, Default)]
struct Table {
    jobs: Mutex<HashMap<String, Arc<Job>>>,
    /// How many Jobs have ended, to put them in the order they ended.
    ended: AtomicU64,
}

/// How a Job ended, as the eviction reads it.
#[derive(Debug)]
struct Finished {
    /// The place of the Job in the order Jobs ended, from 1.
    order: u64,
    /// Whether it exited with code 0.
    success: bool,
}

#[derive(Debug)]
pub(crate) struct Job {
    fingerprint: Fingerprint,
    started_at: Instant,
    /// The PTY child leads its own session, so its pid is also the id of the process group holding its tree.
    pid: Pid,
    output: Arc<Mutex<OutputBuffer>>,
    end: watch::Sender<Option<JobEnd>>,
    /// Set once, before `end` is, when the Job ends.
    finished: OnceLock<Finished>,
}

impl Table {
    fn lock(&self) -> MutexGuard<'_, HashMap<String, Arc<Job>>> {
        self.jobs
            .lock()
            .expect("the job table mutex is never poisoned, no code panics while holding it")
    }

    /// Records that `job` ended with `end`, evicts what the bounds no longer allow, then tells those waiting for it.
    /// The eviction comes first, so that whoever hears of the end finds the table already bounded. It never takes
    /// `job` itself, whose end triggered it: a Job nobody could have read yet is not the one to forget.
    fn finished(&self, job: &Job, end: JobEnd) {
        {
            let mut jobs = self.lock();
            let order = self.ended.fetch_add(1, Ordering::Relaxed) + 1;
            let success = matches!(end, JobEnd::Exited { code: 0 });
            let _ = job.finished.set(Finished { order, success });
            evict(&mut jobs, Some(order));
        }
        job.end.send_replace(Some(end));
    }
}

/// An ended Job, as the eviction ranks it.
#[derive(Debug)]
struct Ended {
    id: String,
    success: bool,
    order: u64,
}

/// Removes from `ended` the Jobs beyond `bound` and returns them: the oldest success goes first, and the oldest
/// failure only once no success is left. The Job of the order `spared` counts toward the bound but is never removed.
fn surplus(ended: &mut Vec<Ended>, bound: usize, spared: Option<u64>) -> Vec<Ended> {
    ended.sort_by_key(|ended| (!ended.success, ended.order));
    let mut excess = ended.len().saturating_sub(bound);
    let mut evicted = Vec::new();
    let mut kept = Vec::new();
    for candidate in ended.drain(..) {
        if excess > 0 && Some(candidate.order) != spared {
            excess -= 1;
            evicted.push(candidate);
        } else {
            kept.push(candidate);
        }
    }
    *ended = kept;
    evicted
}

/// Drops the ended Jobs beyond [`ENDED_JOBS_PER_WORKTREE`] in a Worktree, then beyond [`ENDED_JOBS_TOTAL`] overall,
/// so that a failure nobody has seen stays visible as long as possible. A running Job is never evicted, and neither is
/// the Job of the order `spared`, the one that has just ended.
fn evict(jobs: &mut HashMap<String, Arc<Job>>, spared: Option<u64>) {
    let mut by_worktree: HashMap<Option<&Path>, Vec<Ended>> = HashMap::new();
    for (id, job) in jobs.iter() {
        if let Some(finished) = job.finished.get() {
            by_worktree.entry(job.fingerprint.worktree()).or_default().push(Ended {
                id: id.clone(),
                success: finished.success,
                order: finished.order,
            });
        }
    }
    let mut evicted = Vec::new();
    let mut kept = Vec::new();
    for mut group in by_worktree.into_values() {
        evicted.extend(surplus(&mut group, ENDED_JOBS_PER_WORKTREE, spared));
        kept.extend(group);
    }
    evicted.extend(surplus(&mut kept, ENDED_JOBS_TOTAL, spared));
    for ended in evicted {
        jobs.remove(&ended.id);
    }
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
        let job = spawn(spec, &self.table)?;
        let mut table = self.table();
        loop {
            let id = new_job_id();
            if !table.contains_key(&id) {
                table.insert(id.clone(), job);
                // A Job that ended before it was inserted could not evict when it ended.
                evict(&mut table, None);
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

    /// Every running Job of `worktree`, by identifier in the order they started, for a Worktree release that stops
    /// them with [`Job::stop`].
    pub(crate) fn running_in(&self, worktree: &Path) -> Vec<(String, Arc<Job>)> {
        let mut running: Vec<_> = self
            .table()
            .iter()
            .filter(|(_, job)| job.fingerprint.worktree() == Some(worktree) && job.end.borrow().is_none())
            .map(|(id, job)| (id.clone(), Arc::clone(job)))
            .collect();
        running.sort_by_key(|(_, job)| job.started_at);
        running
    }

    /// Forgets every ended Job of `worktree`, for a Worktree release that has stopped what ran. A running Job stays.
    pub(crate) fn remove_ended_in(&self, worktree: &Path) {
        self.table()
            .retain(|_, job| job.fingerprint.worktree() != Some(worktree) || job.finished.get().is_none());
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

    fn table(&self) -> MutexGuard<'_, HashMap<String, Arc<Job>>> {
        self.table.lock()
    }
}

/// Spawns the Job in a PTY and starts supervising it.
fn spawn(spec: &JobSpec, table: &Arc<Table>) -> Result<Arc<Job>, JobError> {
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
    let log = spec.log.as_ref().and_then(|log| log.try_clone().ok());
    tokio::task::spawn_blocking(move || {
        pump(reader, &pump_output, log);
        let _ = reader_done_tx.send(());
    });
    let waiter = tokio::task::spawn_blocking(move || {
        let end = process_group::wait_for_job(pid);
        // Reaped by `wait_for_job`, the handle has nothing left to release.
        drop(child);
        end
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
        finished: OnceLock::new(),
    });
    tokio::spawn(supervise(
        Arc::clone(&job),
        Arc::clone(table),
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

    /// Follows the output of the Job, from its backlog.
    pub(crate) fn follow(&self) -> Follow {
        output::follow(&self.output)
    }

    /// Stops the whole tree of the Job: `SIGTERM` to its process group, then `SIGKILL` once the stop timeout has passed
    /// with members left, and waits until it has ended.
    pub(crate) async fn stop(&self) {
        // An ended Job no longer holds its group id, which another process may lead by now.
        if self.end.borrow().is_some() {
            return;
        }
        signal_group(self.pid, Signal::SIGTERM);
        if tokio::time::timeout(STOP_TIMEOUT, self.ended()).await.is_err() {
            let pid = self.pid;
            // Signalling a group sleeps between attempts, which must not park a runtime worker.
            let _ = tokio::task::spawn_blocking(move || kill_group(pid)).await;
            self.ended().await;
        }
    }
}

async fn supervise(
    job: Arc<Job>,
    table: Arc<Table>,
    master: Box<dyn MasterPty + Send>,
    mut waiter: JoinHandle<JobEnd>,
    reader_done: oneshot::Receiver<()>,
    timeout: Duration,
) {
    let end = if let Ok(end) = tokio::time::timeout(timeout, &mut waiter).await {
        end.expect("the exit waiter panics only when the daemon lost track of its own child")
    } else {
        process_group::stop_job_tree(job.pid, waiter).await;
        JobEnd::TimedOut {
            timeout_ms: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
        }
    };
    // A Job is bounded: members of its group still running once the leader has ended go with it.
    let pgid = job.pid;
    let _ = tokio::task::spawn_blocking(move || kill_group(pgid)).await;
    let _ = tokio::time::timeout(DRAIN_TIMEOUT, reader_done).await;
    drop(master);
    table.finished(&job, end);
}

/// Reads the terminal of the Job into `output`, and into `log` until a write to it fails: the log is a courtesy that
/// never stops the Job.
fn pump(mut reader: Box<dyn Read + Send>, output: &Mutex<OutputBuffer>, mut log: Option<File>) {
    let mut chunk = [0; 8192];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => return,
            Ok(read) => {
                output::lock(output).push(&chunk[..read]);
                if let Some(file) = &mut log
                    && file.write_all(&chunk[..read]).is_err()
                {
                    log = None;
                }
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Worktree;

    fn topology(worktree: &str) -> Topology {
        Topology {
            git_common_dir: PathBuf::from("/p/repo/.git"),
            worktree: Worktree {
                path: PathBuf::from(worktree),
                slug: "feature".to_owned(),
                main: false,
            },
        }
    }

    /// A Job that has not ended, in the Worktree `worktree` or outside any, held by no process.
    fn job(worktree: Option<&str>) -> Arc<Job> {
        Arc::new(Job {
            fingerprint: Fingerprint {
                topology: worktree.map(topology),
                cwd: PathBuf::from(worktree.unwrap_or("/p/work")),
                command: "true".to_owned(),
            },
            started_at: Instant::now(),
            pid: Pid::from_raw(i32::MAX),
            output: Arc::new(Mutex::new(OutputBuffer::new(output::CAPACITY))),
            end: watch::Sender::new(None),
            finished: OnceLock::new(),
        })
    }

    fn succeeded() -> JobEnd {
        JobEnd::Exited { code: 0 }
    }

    fn failed() -> JobEnd {
        JobEnd::Exited { code: 1 }
    }

    /// Adds a Job under `id` and ends it with `end`, as its supervisor would.
    fn ended(table: &Table, id: &str, worktree: Option<&str>, end: JobEnd) {
        let job = job(worktree);
        table.lock().insert(id.to_owned(), Arc::clone(&job));
        table.finished(&job, end);
    }

    fn known(table: &Table) -> Vec<String> {
        let mut ids: Vec<_> = table.lock().keys().cloned().collect();
        ids.sort();
        ids
    }

    #[test]
    fn the_21st_ended_job_of_a_worktree_evicts_the_oldest_success() {
        let table = Table::default();
        ended(&table, "success", Some("/p/app"), succeeded());
        for n in 1..ENDED_JOBS_PER_WORKTREE {
            ended(&table, &format!("job{n:02}"), Some("/p/app"), failed());
        }
        assert_eq!(
            known(&table).len(),
            ENDED_JOBS_PER_WORKTREE,
            "the bound is reached, not exceeded"
        );

        ended(&table, "next", Some("/p/app"), failed());

        let ids = known(&table);
        assert_eq!(ids.len(), ENDED_JOBS_PER_WORKTREE);
        assert!(!ids.contains(&"success".to_owned()), "the success went first");
        assert!(ids.contains(&"job01".to_owned()), "the older failures stayed");
    }

    #[test]
    fn the_job_that_has_just_ended_is_never_the_one_evicted() {
        let table = Table::default();
        for n in 0..ENDED_JOBS_PER_WORKTREE {
            ended(&table, &format!("job{n:02}"), Some("/p/app"), failed());
        }

        ended(&table, "just-ended", Some("/p/app"), succeeded());

        let ids = known(&table);
        assert_eq!(ids.len(), ENDED_JOBS_PER_WORKTREE);
        assert!(
            ids.contains(&"just-ended".to_owned()),
            "the success that triggered the eviction stays"
        );
        assert!(
            !ids.contains(&"job00".to_owned()),
            "the oldest failure went in its place"
        );
    }

    #[test]
    fn with_only_failures_left_the_oldest_failure_goes() {
        let table = Table::default();
        for n in 0..=ENDED_JOBS_PER_WORKTREE {
            ended(&table, &format!("job{n:02}"), Some("/p/app"), failed());
        }

        let ids = known(&table);
        assert_eq!(ids.len(), ENDED_JOBS_PER_WORKTREE);
        assert!(!ids.contains(&"job00".to_owned()), "the oldest failure went");
        assert!(ids.contains(&"job01".to_owned()));
    }

    #[test]
    fn a_signal_or_a_timeout_is_a_failure_that_outlasts_a_success() {
        let table = Table::default();
        ended(&table, "signaled", Some("/p/app"), JobEnd::Signaled { signal: 9 });
        ended(
            &table,
            "timed_out",
            Some("/p/app"),
            JobEnd::TimedOut { timeout_ms: 1000 },
        );
        for n in 2..=ENDED_JOBS_PER_WORKTREE {
            ended(&table, &format!("job{n:02}"), Some("/p/app"), succeeded());
        }

        let ids = known(&table);
        assert_eq!(ids.len(), ENDED_JOBS_PER_WORKTREE);
        assert!(ids.contains(&"signaled".to_owned()) && ids.contains(&"timed_out".to_owned()));
        assert!(!ids.contains(&"job02".to_owned()), "the oldest success went");
    }

    #[test]
    fn a_running_job_is_never_evicted() {
        let table = Table::default();
        let running = job(Some("/p/app"));
        table.lock().insert("running".to_owned(), Arc::clone(&running));
        for n in 0..(2 * ENDED_JOBS_PER_WORKTREE) {
            ended(&table, &format!("job{n:02}"), Some("/p/app"), succeeded());
        }

        let ids = known(&table);
        assert_eq!(ids.len(), ENDED_JOBS_PER_WORKTREE + 1);
        assert!(ids.contains(&"running".to_owned()));
    }

    #[test]
    fn the_jobs_outside_any_worktree_form_one_group_with_the_same_bound() {
        let table = Table::default();
        for n in 0..=ENDED_JOBS_PER_WORKTREE {
            ended(&table, &format!("loose{n:02}"), None, succeeded());
            ended(&table, &format!("app{n:02}"), Some("/p/app"), succeeded());
        }

        let ids = known(&table);
        assert_eq!(ids.len(), 2 * ENDED_JOBS_PER_WORKTREE);
        assert!(!ids.contains(&"loose00".to_owned()) && !ids.contains(&"app00".to_owned()));
    }

    #[test]
    fn the_total_ceiling_holds_across_several_worktrees() {
        let table = Table::default();
        let worktrees = ENDED_JOBS_TOTAL / ENDED_JOBS_PER_WORKTREE + 2;
        for n in 0..ENDED_JOBS_PER_WORKTREE {
            for worktree in 0..worktrees {
                ended(
                    &table,
                    &format!("w{worktree:02}-{n:02}"),
                    Some(&format!("/p/app{worktree}")),
                    succeeded(),
                );
            }
        }

        let ids = known(&table);
        assert_eq!(ids.len(), ENDED_JOBS_TOTAL);
        // Ended in rounds, the Jobs that went are the oldest ones, from the first rounds.
        assert!(!ids.contains(&"w00-00".to_owned()));
        assert!(ids.contains(&format!("w{:02}-{:02}", worktrees - 1, ENDED_JOBS_PER_WORKTREE - 1)));
    }

    #[test]
    fn a_job_that_ended_before_it_was_inserted_is_still_evicted_once_it_is() {
        let table = Table::default();
        for n in 0..ENDED_JOBS_PER_WORKTREE {
            ended(&table, &format!("job{n:02}"), Some("/p/app"), succeeded());
        }
        let early = job(Some("/p/app"));
        table.finished(&early, succeeded());
        let mut jobs = table.lock();
        jobs.insert("early".to_owned(), early);
        evict(&mut jobs, None);

        assert_eq!(jobs.len(), ENDED_JOBS_PER_WORKTREE);
        assert!(!jobs.contains_key("job00"));
        assert!(jobs.contains_key("early"));
    }

    #[test]
    fn removing_the_ended_jobs_of_a_worktree_keeps_its_running_jobs_and_every_other_worktree() {
        let jobs = Jobs::default();
        ended(&jobs.table, "done", Some("/p/app"), succeeded());
        ended(&jobs.table, "failed", Some("/p/app"), failed());
        ended(&jobs.table, "other", Some("/p/other"), failed());
        ended(&jobs.table, "loose", None, failed());
        jobs.table().insert("running".to_owned(), job(Some("/p/app")));

        jobs.remove_ended_in(Path::new("/p/app"));

        assert_eq!(known(&jobs.table), ["loose", "other", "running"]);
    }
}
