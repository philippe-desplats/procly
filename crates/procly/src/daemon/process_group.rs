//! The Unix side of the spawn and stop boundary (docs/rules/processes.md): process groups of Jobs and Services,
//! the signals that reach their whole tree, and how their leader ended.
//!
//! Every other module goes through this interface and never calls the system itself, so that a Windows port adds a
//! `#[cfg(windows)]` sibling of this module with the same interface, built on Job Objects, and changes nothing else.

use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
use std::process::{Child, Command, ExitStatus};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nix::errno::Errno;
use nix::sys::signal::{Signal, kill, killpg};
use nix::sys::wait::{WaitStatus, waitpid};
/// The id of a process or of a process group, which every module names processes by.
pub(super) use nix::unistd::Pid;
use process_wrap::tokio::{ChildWrapper, CommandWrap, ProcessGroup};
use tokio::task::JoinHandle;

use crate::protocol::{JobEnd, ServiceEnd};

/// One process as the system lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Member {
    pid: Pid,
    ppid: Pid,
    pgid: Pid,
    /// When it started, in a form that only compares with another reading on the same host: what tells it from an
    /// unrelated process that got its pid since.
    started: String,
    /// Ended and waiting for its parent to reap it: no longer there to be signalled.
    zombie: bool,
}

/// The processes of a Service that were seen, and the process group each was seen in.
#[derive(Debug, Default)]
struct Known {
    /// The leader of the current run, whose pid is also the id of the process group it leads.
    leader: Option<Pid>,
    seen: BTreeMap<Pid, Seen>,
}

#[derive(Debug)]
struct Seen {
    pgid: Pid,
    /// `None` for the leader of a run the Daemon just spawned, whose pid is the Daemon's own child until it is waited
    /// for, so that its start is read at the first listing.
    started: Option<String>,
}

impl Known {
    /// Takes a listing of the system in: forgets what ended or got its pid reused, then adds every process whose
    /// parent is in the tree, and finally whatever those started, except a process of the group of the Daemon.
    fn absorb(&mut self, listing: &[Member], daemon_group: Pid) {
        let alive: BTreeMap<Pid, &Member> = listing
            .iter()
            .filter(|member| !member.zombie)
            .map(|member| (member.pid, member))
            .collect();
        // A pid that is gone, or that shows another start than the one seen, is not the process that was seen: it
        // leaves the tree, and so does everything that only hung from it.
        self.seen.retain(|pid, seen| {
            alive
                .get(pid)
                .is_some_and(|member| seen.started.as_ref().is_none_or(|started| *started == member.started))
        });
        for (pid, seen) in &mut self.seen {
            if let Some(member) = alive.get(pid) {
                seen.pgid = member.pgid;
                seen.started.get_or_insert_with(|| member.started.clone());
            }
        }
        // Whatever moved into the group of the Daemon is the Daemon's to keep, never signalled as a group.
        self.seen.retain(|_, seen| seen.pgid != daemon_group);
        loop {
            let found: Vec<&Member> = alive
                .values()
                .copied()
                .filter(|member| {
                    !self.seen.contains_key(&member.pid)
                        && self.seen.contains_key(&member.ppid)
                        && member.pgid != daemon_group
                })
                .collect();
            if found.is_empty() {
                return;
            }
            for member in found {
                self.seen.insert(
                    member.pid,
                    Seen {
                        pgid: member.pgid,
                        started: Some(member.started.clone()),
                    },
                );
            }
        }
    }

    /// The process groups of the processes seen alive in the last listing.
    fn groups(&self) -> BTreeSet<Pid> {
        self.seen.values().map(|seen| seen.pgid).collect()
    }
}

/// How long a stopped tree gets between `SIGTERM` and `SIGKILL`.
pub(super) const STOP_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the output of an ended Job or Service is still read, when a descendant that left its group keeps it
/// open.
pub(super) const DRAIN_TIMEOUT: Duration = Duration::from_secs(1);
/// How many times a group is sent `SIGKILL` before giving up on members that keep appearing.
const KILL_ATTEMPTS: u32 = 100;
/// How often a stopped group that is not a child of the Daemon is looked at, to tell whether it is gone.
const GONE_POLL: Duration = Duration::from_millis(20);

/// Sends `SIGKILL` to the group until it is empty, so that a member forked while the first signal was in flight
/// goes too. The group keeps its id reserved while any member lives, so this never reaches an unrelated process.
pub(super) fn kill_group(pgid: Pid) {
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

pub(super) fn signal_group(pgid: Pid, signal: Signal) {
    match killpg(pgid, signal) {
        Ok(()) | Err(Errno::ESRCH) => {}
        Err(error) => eprintln!("cannot send {signal} to process group {pgid}: {error}"),
    }
}

/// What tells one process from another that later got the same pid: its process group and when it started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Identity {
    pub(super) pgid: Pid,
    /// When the process started, in a form that only compares with another reading on the same host.
    pub(super) started: String,
}

/// The identity of the live process `pid`, `None` when no such process exists or its start cannot be read.
///
/// Linux keeps the start in `/proc` as clock ticks after boot, which stay the same when the wall clock is set. Its
/// `ps` would compute a date from them that moves with the clock, and a Service would then no longer be recognized.
/// macOS keeps the date itself, which `ps` prints, with the locale and the time zone fixed so that a Daemon started
/// from a shell and one started by the session manager read the same.
#[cfg(target_os = "linux")]
pub(super) fn identity(pid: Pid) -> Option<Identity> {
    parse_proc_stat(&std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)
}

/// See the Linux version above.
#[cfg(not(target_os = "linux"))]
pub(super) fn identity(pid: Pid) -> Option<Identity> {
    let output = std::process::Command::new("ps")
        .args(["-o", "pgid=,lstart=", "-p", &pid.to_string()])
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .output()
        .ok()
        .filter(|output| output.status.success())?;
    parse_identity(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(any(target_os = "linux", test))]
/// Reads the process group and the start of `/proc/<pid>/stat`, the 5th and 22nd fields, counted after the name of
/// the command, which may hold spaces and parentheses itself.
fn parse_proc_stat(stat: &str) -> Option<Identity> {
    let (_, fields) = stat.rsplit_once(')')?;
    // The state is the 3rd field, so the 5th is 2 after it and the 22nd 19 after it.
    let fields: Vec<&str> = fields.split_whitespace().collect();
    let pgid = fields.get(2)?.parse().ok()?;
    let started: u64 = fields.get(19)?.parse().ok()?;
    Some(Identity {
        pgid: Pid::from_raw(pgid),
        started: format!("{started} ticks after boot"),
    })
}

#[cfg(any(not(target_os = "linux"), test))]
/// Reads the line `ps -o pgid=,lstart=` prints, such as `4242 Sun Oct 18 20:52:01 2026`.
fn parse_identity(line: &str) -> Option<Identity> {
    let (pgid, started) = line.trim().split_once(char::is_whitespace)?;
    let started = started.trim();
    if started.is_empty() {
        return None;
    }
    Some(Identity {
        pgid: Pid::from_raw(pgid.parse().ok()?),
        started: started.to_owned(),
    })
}

/// Whether the process `pid` still exists. One that is not a child of the Daemon is reaped by init as it ends.
pub(super) fn is_alive(pid: Pid) -> bool {
    !matches!(kill(pid, None), Err(Errno::ESRCH))
}

/// Whether the process group `pgid` still has a member.
pub(super) fn group_exists(pgid: Pid) -> bool {
    !matches!(killpg(pgid, None), Err(Errno::ESRCH))
}

/// Stops a process group that is not a child of the Daemon, so that it cannot be waited for: `SIGTERM` to the whole
/// group, then `SIGKILL` once [`STOP_TIMEOUT`] has passed with members left, and returns once it is empty.
pub(super) async fn stop_group(pgid: Pid) {
    signal_group(pgid, Signal::SIGTERM);
    let deadline = tokio::time::Instant::now() + STOP_TIMEOUT;
    while group_exists(pgid) && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(GONE_POLL).await;
    }
    // Sending signals sleeps between attempts, which must not park a runtime worker.
    let _ = tokio::task::spawn_blocking(move || kill_group(pgid)).await;
}

/// The process group of the Daemon itself, whose members Procly never counts as Foreign processes.
pub(super) fn own_group() -> Pid {
    nix::unistd::getpgrp()
}

/// Makes the spawned command, which serves a Service without belonging to its tree, lead a process group of its own,
/// so that stopping the Service never signals it and it outlives the Daemon.
pub(super) fn detach(command: &mut tokio::process::Command) {
    command.process_group(0);
}

/// Makes the spawned command lead a process group of its own, so that a signal to the group reaches its whole tree.
pub(super) fn lead_group(command: &mut CommandWrap) {
    command.wrap(ProcessGroup::leader());
}

/// Like [`lead_group`], for a command the Daemon spawns and waits for itself, without the async runtime.
pub(super) fn lead_group_blocking(command: &mut Command) {
    command.process_group(0);
}

/// Kills the whole tree of a child spawned after [`lead_group_blocking`] and reaps it: the group first, so that no
/// descendant outlives it, the leader so that it can be waited for, then whatever the leader forked meanwhile.
/// Blocking.
pub(super) fn kill_tree_blocking(child: &mut Child) {
    let group = i32::try_from(child.id()).map(Pid::from_raw).ok();
    if let Some(group) = group {
        signal_group(group, Signal::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
    if let Some(group) = group {
        kill_group(group);
    }
}

/// The tree of a Service: its leader and everything the leader started, found through the parent of each process, in
/// whatever process group it runs. A tool such as turbo gives each of its tasks a group of its own, so the group of
/// the leader holds only the tool itself.
///
/// It remembers every process it saw, by pid and start, so that a task whose parent ended and that init adopted is
/// still part of it. Nothing is ever signalled that a listing taken just before did not show with the start it was
/// seen with: a pid some unrelated process got since is not part of the tree.
///
/// Clones share the tree. Every method that reads the system blocks, so an async task calls it from a blocking
/// thread.
#[derive(Debug, Clone, Default)]
pub(super) struct Tree {
    known: Arc<Mutex<Known>>,
}

impl Tree {
    /// The tree of the live leader `leader`, which leads the process group of the same id. Blocking.
    pub(super) fn of(leader: Pid) -> Self {
        let tree = Self::default();
        tree.begin(leader);
        tree.refresh();
        tree
    }

    /// Starts the tree over with the leader `leader` of a run the Daemon just spawned, which is its child until it
    /// waits for it. Its start is read by the next [`Tree::refresh`].
    pub(super) fn begin(&self, leader: Pid) {
        let mut known = self.known();
        known.leader = Some(leader);
        known.seen = BTreeMap::from([(
            leader,
            Seen {
                pgid: leader,
                started: None,
            },
        )]);
    }

    /// Reads the system and brings the tree up to date. A listing that cannot be read leaves it as it was, and
    /// reports `false`. Blocking.
    pub(super) fn refresh(&self) -> bool {
        let Some(listing) = listing() else {
            return false;
        };
        self.known().absorb(&listing, own_group());
        true
    }

    /// The process groups the processes of the tree were last seen in.
    pub(super) fn groups(&self) -> BTreeSet<Pid> {
        self.known().groups()
    }

    /// Whether no process of the tree was seen alive in the last listing.
    fn is_empty(&self) -> bool {
        self.known().seen.is_empty()
    }

    /// Sends `signal` to the process group of the leader alone, if the leader's group still has a process seen in a
    /// listing taken now. Blocking.
    fn signal_leader_group(&self, signal: Signal) {
        if !self.refresh() {
            return;
        }
        let known = self.known();
        if let Some(leader) = known.leader.filter(|leader| known.groups().contains(leader)) {
            signal_group(leader, signal);
        }
    }

    /// Sends `SIGKILL` to every group of the tree until none has a process left, so that a member forked while the
    /// first signal was in flight goes too. Blocking.
    pub(super) fn kill(&self) {
        for _ in 0..KILL_ATTEMPTS {
            // Without a listing nothing proves that a pid is still the process that was seen.
            if !self.refresh() {
                eprintln!("cannot read the process list, the tree of the process group was left alone");
                return;
            }
            let groups = self.groups();
            if groups.is_empty() {
                return;
            }
            for group in groups {
                signal_group(group, Signal::SIGKILL);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        eprintln!("the tree of a service still has members after {KILL_ATTEMPTS} SIGKILL");
    }

    fn known(&self) -> std::sync::MutexGuard<'_, Known> {
        self.known
            .lock()
            .expect("the tree mutex is never poisoned, no code panics while holding it")
    }
}

/// Brings `tree` up to date from a blocking thread.
pub(super) async fn refresh(tree: &Tree) {
    let tree = tree.clone();
    let _ = tokio::task::spawn_blocking(move || tree.refresh()).await;
}

/// Sends `SIGKILL` to the whole tree from a blocking thread, see [`Tree::kill`].
pub(super) async fn kill_tree(tree: &Tree) {
    let tree = tree.clone();
    let _ = tokio::task::spawn_blocking(move || tree.kill()).await;
}

/// Returns once no process of `tree` runs.
async fn gone(tree: &Tree) {
    loop {
        refresh(tree).await;
        if tree.is_empty() {
            return;
        }
        tokio::time::sleep(GONE_POLL).await;
    }
}

/// Stops the tree of a Service whose leader is a child of the Daemon: `SIGTERM` to the group of the leader, then a
/// wait until the whole tree is gone, and `SIGKILL` to every group of the tree that still has a process once the stop
/// timeout has passed.
///
/// The leader ending is not the end of the tree: a tool such as turbo outlives the `bun run dev` that started it by a
/// few tens of milliseconds, the time it needs to stop the tasks it keeps in groups of their own.
pub(super) async fn stop_tree(child: &mut dyn ChildWrapper, tree: &Tree) {
    // Before the signal, as the leader may end within milliseconds of it and take the links to its tasks along.
    refresh(tree).await;
    if let Err(error) = child.signal(Signal::SIGTERM as i32) {
        eprintln!("cannot send SIGTERM to process group {:?}: {error}", child.id());
    }
    let drained = tokio::time::timeout(STOP_TIMEOUT, async {
        let _ = child.wait().await;
        gone(tree).await;
    })
    .await;
    if drained.is_err() {
        // Only a leader that was not waited for yet is certainly its group's: the group of one that was is the tree's
        // to kill, from a listing.
        if matches!(child.try_wait(), Ok(None))
            && let Err(error) = child.start_kill()
        {
            eprintln!("cannot kill process group {:?}: {error}", child.id());
        }
        kill_tree(tree).await;
        let _ = child.wait().await;
    }
}

/// Stops the tree of a Service whose leader is no child of the Daemon, so that it cannot be waited for: the same
/// sequence as [`stop_tree`], and returns once the tree is gone.
pub(super) async fn stop_detached(tree: &Tree) {
    let leader = tree.clone();
    let _ = tokio::task::spawn_blocking(move || leader.signal_leader_group(Signal::SIGTERM)).await;
    let _ = tokio::time::timeout(STOP_TIMEOUT, gone(tree)).await;
    kill_tree(tree).await;
}

/// Reads every process of the system.
#[cfg(not(target_os = "linux"))]
fn listing() -> Option<Vec<Member>> {
    let output = Command::new("ps")
        .args(["-axo", "pid=,ppid=,pgid=,stat=,lstart="])
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .output()
        .ok()
        .filter(|output| output.status.success())?;
    Some(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(parse_member)
            .collect(),
    )
}

/// See the other version above: the files of `/proc` hold the start as clock ticks after boot.
#[cfg(target_os = "linux")]
fn listing() -> Option<Vec<Member>> {
    let entries = std::fs::read_dir("/proc").ok()?;
    Some(
        entries
            .filter_map(|entry| {
                let entry = entry.ok()?;
                let pid = entry.file_name().to_str()?.parse().ok()?;
                // A process that ended since the directory was read has no file any more.
                let stat = std::fs::read_to_string(entry.path().join("stat")).ok()?;
                parse_proc_member(pid, &stat)
            })
            .collect(),
    )
}

#[cfg(any(not(target_os = "linux"), test))]
/// Reads a line of `ps -axo pid=,ppid=,pgid=,stat=,lstart=`, such as `4242 1 4242 Ss Sun Oct 18 20:52:01 2026`.
fn parse_member(line: &str) -> Option<Member> {
    let mut fields = line.split_whitespace();
    let mut number = || fields.next()?.parse().ok().map(Pid::from_raw);
    let [pid, parent, group] = [number()?, number()?, number()?];
    let stat = fields.next()?;
    let started = fields.collect::<Vec<_>>().join(" ");
    if started.is_empty() {
        return None;
    }
    Some(Member {
        pid,
        ppid: parent,
        pgid: group,
        started,
        zombie: stat.starts_with('Z'),
    })
}

#[cfg(any(target_os = "linux", test))]
/// Reads the state, the parent, the process group and the start of `/proc/<pid>/stat`, the 3rd, 4th, 5th and 22nd
/// fields, counted after the name of the command like [`parse_proc_stat`] does.
fn parse_proc_member(pid: i32, stat: &str) -> Option<Member> {
    let (_, fields) = stat.rsplit_once(')')?;
    let fields: Vec<&str> = fields.split_whitespace().collect();
    Some(Member {
        pid: Pid::from_raw(pid),
        ppid: Pid::from_raw(fields.get(1)?.parse().ok()?),
        pgid: Pid::from_raw(fields.get(2)?.parse().ok()?),
        started: format!("{} ticks after boot", fields.get(19)?.parse::<u64>().ok()?),
        zombie: fields.first()?.starts_with('Z'),
    })
}

/// How the leader of a Service ended, from its exit status.
pub(super) fn service_end(status: ExitStatus) -> ServiceEnd {
    match status.signal() {
        Some(signal) => ServiceEnd::Signaled { signal },
        None => ServiceEnd::Exited {
            code: status.code().expect("an exit status without a signal carries a code"),
        },
    }
}

/// Reaps the leader of a Job with `waitpid`, which tells an exit code from a signal, unlike `portable_pty::Child`,
/// and tells how it ended. Blocking.
pub(super) fn wait_for_job(pid: Pid) -> JobEnd {
    loop {
        match waitpid(pid, None) {
            Ok(WaitStatus::Signaled(_, signal, _)) => return JobEnd::Signaled { signal: signal as i32 },
            Ok(WaitStatus::Exited(_, code)) => return JobEnd::Exited { code },
            Ok(_) | Err(Errno::EINTR) => {}
            Err(error) => panic!("cannot wait for job process {pid}, which only the daemon reaps: {error}"),
        }
    }
}

/// Stops the leader of the process group of a Job, reaped by `waiter`: `SIGTERM` to the whole group, then `SIGKILL`
/// after the stop timeout.
pub(super) async fn stop_job_tree(pgid: Pid, mut waiter: JoinHandle<JobEnd>) {
    signal_group(pgid, Signal::SIGTERM);
    if tokio::time::timeout(STOP_TIMEOUT, &mut waiter).await.is_err() {
        signal_group(pgid, Signal::SIGKILL);
        let _ = waiter.await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_group_and_the_start_time_ps_prints() {
        assert_eq!(
            parse_identity(" 4242 Sun Oct 18 20:52:01 2026\n"),
            Some(Identity {
                pgid: Pid::from_raw(4242),
                started: "Sun Oct 18 20:52:01 2026".to_owned()
            })
        );
    }

    #[test]
    fn reads_the_group_and_the_start_time_linux_keeps_in_proc() {
        // The name of the command may hold spaces and parentheses, the fields after its last one are the numbers.
        let stat = "4242 (my (odd) cmd) S 1 4242 4242 0 -1 4194560 120 0 0 0 1 2 0 0 20 0 1 0 987654 1234 56 \
                    18446744073709551615\n";
        assert_eq!(
            parse_proc_stat(stat),
            Some(Identity {
                pgid: Pid::from_raw(4242),
                started: "987654 ticks after boot".to_owned()
            })
        );
        assert_eq!(parse_proc_stat("4242 (cmd) S 1"), None);
    }

    fn member(pid: i32, parent: i32, group: i32, started: &str) -> Member {
        Member {
            pid: Pid::from_raw(pid),
            ppid: Pid::from_raw(parent),
            pgid: Pid::from_raw(group),
            started: started.to_owned(),
            zombie: false,
        }
    }

    /// The tree of the leader 100, seen with its first listing, whose Daemon runs in the group 7.
    fn tree_of_100(listing: &[Member]) -> Known {
        let mut known = Known::default();
        known.seen.insert(
            Pid::from_raw(100),
            Seen {
                pgid: Pid::from_raw(100),
                started: None,
            },
        );
        known.absorb(listing, Pid::from_raw(7));
        known
    }

    fn groups(pgids: &[i32]) -> BTreeSet<Pid> {
        pgids.iter().copied().map(Pid::from_raw).collect()
    }

    #[test]
    fn a_task_in_a_group_of_its_own_is_found_through_its_parents() {
        // 100 leads the group of the Service, 101 is turbo, 102 and 103 are tasks that lead groups of their own.
        let known = tree_of_100(&[
            member(100, 7, 100, "t0"),
            member(101, 100, 100, "t1"),
            member(102, 101, 102, "t2"),
            member(103, 102, 102, "t3"),
            member(900, 1, 900, "t9"),
        ]);

        assert_eq!(known.groups(), groups(&[100, 102]));
    }

    #[test]
    fn an_orphan_reparented_to_init_stays_in_the_tree() {
        let mut known = tree_of_100(&[member(100, 7, 100, "t0"), member(102, 100, 102, "t2")]);

        // The leader ended and its task now belongs to init, which no parent link of the tree tells any more.
        known.absorb(&[member(102, 1, 102, "t2")], Pid::from_raw(7));

        assert_eq!(known.groups(), groups(&[102]));
    }

    #[test]
    fn a_process_of_the_daemons_group_never_enters_the_tree() {
        // 105 is a child of the leader that was put in the group of the Daemon, 106 is started by it.
        let known = tree_of_100(&[
            member(100, 7, 100, "t0"),
            member(105, 100, 7, "t5"),
            member(106, 105, 106, "t6"),
        ]);

        assert_eq!(known.groups(), groups(&[100]));
    }

    #[test]
    fn a_reused_pid_leaves_the_tree_with_its_group_and_what_it_started() {
        let mut known = tree_of_100(&[member(100, 7, 100, "t0"), member(102, 100, 102, "t2")]);

        // 102 ended, and an unrelated process got its pid, which has a child of its own.
        known.absorb(
            &[
                member(100, 7, 100, "t0"),
                member(102, 1, 102, "later"),
                member(110, 102, 102, "later2"),
            ],
            Pid::from_raw(7),
        );

        assert_eq!(known.groups(), groups(&[100]));
    }

    #[test]
    fn a_process_that_ended_leaves_the_tree_and_a_zombie_is_not_one_that_runs() {
        let mut known = tree_of_100(&[
            member(100, 7, 100, "t0"),
            member(102, 100, 102, "t2"),
            member(103, 100, 103, "t3"),
        ]);
        let mut zombie = member(103, 100, 103, "t3");
        zombie.zombie = true;

        known.absorb(&[member(100, 7, 100, "t0"), zombie], Pid::from_raw(7));

        assert_eq!(known.groups(), groups(&[100]));
    }

    #[test]
    fn a_leader_that_is_not_listed_yet_is_kept_until_it_is() {
        let mut known = Known::default();
        known.seen.insert(
            Pid::from_raw(100),
            Seen {
                pgid: Pid::from_raw(100),
                started: None,
            },
        );

        known.absorb(&[member(100, 7, 100, "t0")], Pid::from_raw(7));
        assert_eq!(known.groups(), groups(&[100]));
        // A pid that shows another start later is another process.
        known.absorb(&[member(100, 1, 100, "other")], Pid::from_raw(7));

        assert_eq!(known.groups(), BTreeSet::new());
    }

    #[test]
    fn reads_a_process_of_the_ps_listing_with_its_start_and_whether_it_ended() {
        assert_eq!(
            parse_member(" 4242     1  4240 Ss   Sun Oct 18 20:52:01 2026\n"),
            Some(member(4242, 1, 4240, "Sun Oct 18 20:52:01 2026"))
        );
        assert!(parse_member("4242 1 4240 Z Sun Oct 18 20:52:01 2026").is_some_and(|member| member.zombie));
        assert_eq!(parse_member("4242 1 4240 Ss"), None);
        assert_eq!(parse_member("pid ppid pgid stat started"), None);
    }

    #[test]
    fn reads_a_process_of_proc_with_its_parent_group_and_start() {
        let stat = "4242 (my (odd) cmd) S 1 4240 4240 0 -1 4194560 120 0 0 0 1 2 0 0 20 0 1 0 987654 1234 56 \
                    18446744073709551615\n";
        assert_eq!(
            parse_proc_member(4242, stat),
            Some(member(4242, 1, 4240, "987654 ticks after boot"))
        );
        assert!(parse_proc_member(4242, &stat.replace(") S ", ") Z ")).is_some_and(|member| member.zombie));
        assert_eq!(parse_proc_member(4242, "4242 (cmd) S 1"), None);
    }

    #[test]
    fn reads_no_identity_from_an_empty_or_partial_line() {
        assert_eq!(parse_identity(""), None);
        assert_eq!(parse_identity("4242"), None);
        assert_eq!(parse_identity("4242 "), None);
        assert_eq!(parse_identity("pgid Sun Oct 18 20:52:01 2026"), None);
    }
}
