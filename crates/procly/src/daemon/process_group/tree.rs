//! The tree of a Service: every process that started from its leader and did not leave its session.
//!
//! A Service leads a session of its own, so everything it starts belongs to that session, whatever process group a
//! tool puts its tasks in (turbo runs each in one of its own) and whoever adopts them once their parent ended. Finding
//! the tree is a matter of reading the session of each process, never of following parents, so that a task whose
//! parent ended a few milliseconds after starting it is not missed. A process that calls `setsid` leaves the tree,
//! which is what a daemon meant to outlive the Service does.
//!
//! A Service that an older Daemon started leads a process group only, and is known by that group.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use nix::sys::signal::Signal;
use process_wrap::tokio::ChildWrapper;

use super::{
    GONE_POLL, KILL_ATTEMPTS, Pid, STOP_TIMEOUT, group_exists, is_alive, kill_group, normalized, own_group,
    signal_group,
};

/// How long a listing of the processes is shared by the Services that ask for one, so that N Services looking at
/// their tree within the same few milliseconds cost one listing.
const SHARED_FOR: Duration = Duration::from_millis(40);

/// How often a stopping tree is listed again to find the groups that appeared since, while the groups known are
/// probed every [`GONE_POLL`]. A listing costs a `ps` of the whole system, the probe of a group costs one system call.
const RELIST: Duration = Duration::from_millis(500);

/// One process as the system lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Member {
    pid: Pid,
    pgid: Pid,
    sid: Pid,
    /// When it started, in a form that only compares with another reading on the same host: what tells it from an
    /// unrelated process that got its pid since.
    started: String,
    /// Ended and waiting for its parent to reap it: no longer there to be signalled.
    zombie: bool,
}

/// What a tree is made of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    /// The processes of the session that the leader `Pid` leads.
    Session(Pid),
    /// The processes of the process group that the leader `Pid` leads, for a Service an older Daemon started.
    Group(Pid),
    /// Either of the 2 above, not known yet: the leader `Pid` was found running but the processes could not be listed to
    /// tell whether it leads a session. Until a listing shows it, the tree is its process group, which is all that is
    /// certain of it.
    Undecided(Pid),
}

impl Scope {
    fn leader(self) -> Pid {
        match self {
            Self::Session(leader) | Self::Group(leader) | Self::Undecided(leader) => leader,
        }
    }

    fn holds(self, member: &Member) -> bool {
        match self {
            Self::Session(leader) => member.sid == leader,
            Self::Group(leader) | Self::Undecided(leader) => member.pgid == leader,
        }
    }
}

/// Where the tree stands with its leader.
///
/// The id of a session or of a group is the pid of its leader, which the system does not hand to another process while
/// any process still has that id as its session or group. That holds only while some process had it without a break:
/// once the session or the group has been empty, an unrelated process can get the pid, lead a session of its own and
/// start children, and everything in that session is then its own. The tree therefore takes a session or a group in
/// through the id of the leader only while the leader itself shows in the listing with the start it was recorded with.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Standing {
    /// A leader that a previous Daemon recorded and that no listing has shown yet.
    #[default]
    Unseen,
    /// The listing shows the leader with its recorded start, a zombie not yet reaped included: its pid is still its own.
    /// `grace` is for a leader the Daemon itself started and that it reaps: it is the only one that can tell that it
    /// ended a moment ago, so the first listing taken after that takes its session in, whatever was listed before.
    Leading { grace: bool },
    /// The leader ended or another process got its pid, and the tree is made of the processes it knew, by pid and start.
    Orphaned,
}

#[derive(Debug, Default)]
struct Known {
    scope: Option<Scope>,
    /// When the leader started, once known. It tells the leader from a process that got its pid since.
    leader_started: Option<String>,
    /// The process groups of the processes seen alive in the last listing.
    groups: BTreeSet<Pid>,
    /// Whether a listing has been read since the tree began.
    listed: bool,
    standing: Standing,
    /// The processes of the tree in the last listing that held them, by pid and start: what lets the tree tell the
    /// processes it knew from an unrelated one that got the id of its session once the leader was gone.
    members: BTreeMap<Pid, String>,
}

impl Known {
    /// The tree of the leader `leader` that the Daemon just started in a session of its own and will reap.
    fn spawned(leader: Pid) -> Self {
        Self {
            scope: Some(Scope::Session(leader)),
            groups: BTreeSet::from([leader]),
            standing: Standing::Leading { grace: true },
            ..Self::default()
        }
    }

    /// The tree of the leader `leader` that a previous Daemon recorded, with the start `started` if the record holds
    /// one, seen in `listing` if the processes could be listed. A leader found running whose processes cannot be listed
    /// is still the Service, which keeps its process group until a listing says more: never dropped on a failed reading.
    fn recorded(leader: Pid, started: Option<&str>, listing: Option<&[Member]>, daemon_group: Pid) -> Self {
        let mut known = Self {
            scope: Some(Scope::Undecided(leader)),
            leader_started: started.map(normalized),
            groups: BTreeSet::from([leader]),
            ..Self::default()
        };
        if let Some(listing) = listing {
            known.absorb(listing, daemon_group);
        }
        known
    }

    /// Takes a listing of the system in: the groups of the processes of the tree, now.
    fn absorb(&mut self, listing: &[Member], daemon_group: Pid) {
        let Some(mut scope) = self.scope else { return };
        self.listed = true;
        let leader = scope.leader();
        // A zombie counts: the pid of a leader that is not reaped yet is still reserved, and so is the id of its session.
        let row = listing.iter().find(|member| member.pid == leader);
        let seen = row.filter(|row| {
            self.leader_started
                .as_ref()
                .is_none_or(|started| *started == row.started)
        });
        if let Some(row) = seen {
            // A leader that does not lead its own session was started before Services did.
            if matches!(scope, Scope::Undecided(_)) {
                scope = if row.sid == leader {
                    Scope::Session(leader)
                } else {
                    Scope::Group(leader)
                };
                self.scope = Some(scope);
            }
            if scope.holds(row) {
                self.leader_started.get_or_insert_with(|| row.started.clone());
                let grace = matches!(self.standing, Standing::Leading { grace: true });
                self.standing = Standing::Leading { grace };
                self.keep(
                    listing
                        .iter()
                        .filter(|member| Self::in_tree(scope, member, daemon_group)),
                );
                return;
            }
        }
        // The leader is not in the listing with its start: it ended, or some other process got its pid. Whatever holds
        // its id now is that process's, unless a process the tree already knew has held it all along.
        let candidates: Vec<&Member> = listing
            .iter()
            .filter(|member| Self::in_tree(scope, member, daemon_group))
            .collect();
        let known_before = candidates
            .iter()
            .any(|member| self.members.get(&member.pid) == Some(&member.started));
        let adopted = match self.standing {
            Standing::Leading { grace: true } if row.is_none() => true,
            Standing::Unseen => {
                eprintln!(
                    "the leader {leader} of a recorded tree was not seen running with the start of the record, so \
                     nothing in its session or process group is taken for the tree or signalled"
                );
                false
            }
            Standing::Leading { .. } | Standing::Orphaned => {
                // Said once, when the tree loses the last process it knew, and not at every listing of a stop.
                let said = self.standing == Standing::Orphaned && self.members.is_empty();
                if !known_before && !candidates.is_empty() && !said {
                    eprintln!(
                        "the leader {leader} of a tree ended and none of the processes the tree knew is left, so \
                         what holds its session or process group now is taken for another tree's and left alone"
                    );
                }
                known_before
            }
        };
        self.standing = Standing::Orphaned;
        if adopted {
            self.keep(candidates.into_iter());
        } else {
            self.groups.clear();
            self.members.clear();
        }
    }

    /// Whether `member` is in the tree of `scope`, a process of the Daemon's own group never being.
    fn in_tree(scope: Scope, member: &Member, daemon_group: Pid) -> bool {
        !member.zombie && scope.holds(member) && member.pgid != daemon_group
    }

    /// Makes the tree the processes `members`, which the last listing showed in it.
    fn keep<'a>(&mut self, members: impl Iterator<Item = &'a Member> + Clone) {
        self.groups = members.clone().map(|member| member.pgid).collect();
        self.members = members.map(|member| (member.pid, member.started.clone())).collect();
    }
}

/// The tree of a Service. Clones share it. Every method that reads the system blocks, so an async task calls it from
/// a blocking thread.
#[derive(Debug, Clone, Default)]
pub(in crate::daemon) struct Tree {
    known: Arc<Mutex<Known>>,
}

impl Tree {
    /// Starts the tree over with the leader `leader` of a run the Daemon just spawned in a session of its own. Until
    /// the next listing, the tree is the group of the leader.
    pub(in crate::daemon) fn begin(&self, leader: Pid) {
        *self.known() = Known::spawned(leader);
    }

    /// Records the start of the leader of the run the Daemon just spawned, read right after the spawn, so that the tree
    /// never depends on a first listing that may come after the leader was reaped and its pid given to another process.
    pub(in crate::daemon) fn note_start(&self, started: Option<String>) {
        if let Some(started) = started {
            self.known().leader_started.get_or_insert(started);
        }
    }

    /// Starts the tree over with the leader `leader` that a previous Daemon started and recorded with the start
    /// `started`, which is the process that leader was when the caller read it. The tree is empty if the pid now belongs
    /// to another process. Blocking.
    pub(in crate::daemon) fn begin_recorded(&self, leader: Pid, started: Option<&str>) {
        let listing = listing(Duration::ZERO);
        *self.known() = Known::recorded(leader, started, listing.as_deref().map(Vec::as_slice), own_group());
    }

    /// Reads the processes of the system, or reuses a reading a few milliseconds old that another Service took, and
    /// brings the tree up to date. A listing that cannot be read leaves the tree as it was and reports `false`.
    /// Blocking.
    pub(in crate::daemon) fn refresh(&self) -> bool {
        self.refresh_within(SHARED_FOR)
    }

    /// Like [`Tree::refresh`], from a reading taken now: what a signal is sent from. Blocking.
    fn refresh_now(&self) -> bool {
        self.refresh_within(Duration::ZERO)
    }

    /// Like [`Tree::refresh`], from a reading at most `age` old. Blocking.
    pub(in crate::daemon) fn refresh_within(&self, age: Duration) -> bool {
        let Some(listing) = listing(age) else {
            return false;
        };
        self.known().absorb(&listing, own_group());
        true
    }

    /// The process groups of the processes of the tree in the last listing.
    pub(in crate::daemon) fn groups(&self) -> BTreeSet<Pid> {
        self.known().groups.clone()
    }

    /// Whether the tree of a leader a previous Daemon recorded is still only its process group, because no listing of the
    /// processes has been read since.
    pub(in crate::daemon) fn is_unlisted(&self) -> bool {
        let known = self.known();
        matches!(known.scope, Some(Scope::Undecided(_))) && !known.listed
    }

    /// Whether no process of the tree was seen alive in the last listing.
    fn is_empty(&self) -> bool {
        self.known().groups.is_empty()
    }

    fn leader(&self) -> Option<Pid> {
        self.known().scope.map(Scope::leader)
    }

    /// Sends `signal` to the group of the leader, if a listing taken now shows a process in it. When the processes cannot
    /// be listed, to the group of a leader that is still the process the tree began with. Blocking.
    fn signal_leader_group(&self, signal: Signal) {
        let Some(leader) = self.leader() else { return };
        let signalled = if self.refresh_now() {
            self.groups().contains(&leader)
        } else {
            self.leader_is_ours()
        };
        if signalled {
            signal_group(leader, signal);
        }
    }

    /// Whether the group of the leader holds the processes the tree began with, when the processes cannot be listed.
    ///
    /// A leader that runs is the one the tree began with if it leads its group and still has the start it was seen with,
    /// and a leader whose start was never seen proves nothing. A leader that ended proves nothing either, since its group
    /// may have been empty since and its id given to another process, except for one the Daemon started and reaped itself,
    /// a moment ago. Blocking.
    fn leader_is_ours(&self) -> bool {
        let Some(leader) = self.leader() else {
            return false;
        };
        match super::identity(leader) {
            Some(identity) => {
                identity.pgid == leader
                    && self
                        .known()
                        .leader_started
                        .as_ref()
                        .is_some_and(|started| *started == identity.started)
            }
            None => matches!(self.known().standing, Standing::Leading { grace: true }) && !is_alive(leader),
        }
    }

    /// Whether every process group of the tree in the last listing is empty, or the group of the leader is when no
    /// listing could be taken. A reading of the system is taken to tell it, when the groups known are all empty or
    /// `since` is older than [`RELIST`], and then dated again. Blocking.
    fn is_gone(&self, since: &mut Instant) -> bool {
        let known = self.groups();
        if known.iter().any(|group| group_exists(*group)) && since.elapsed() < RELIST {
            return false;
        }
        *since = Instant::now();
        if self.refresh_now() {
            self.is_empty()
        } else {
            self.leader().is_none_or(|leader| !group_exists(leader))
        }
    }

    /// Sends `signal` to every group of the tree but the group of the leader, which was signalled before, from a
    /// listing taken now. Blocking.
    fn signal_other_groups(&self, signal: Signal) {
        if !self.refresh_now() {
            return;
        }
        let leader = self.leader();
        for group in self.groups() {
            if Some(group) != leader {
                signal_group(group, signal);
            }
        }
    }

    /// Sends `SIGKILL` to every group of the tree until none has a process left, so that a member forked while the
    /// first signal was in flight goes too. Blocking.
    ///
    /// When the processes cannot be listed, nothing proves that a group of the tree is still the one that was seen,
    /// except the group of a leader that still runs with the start it was seen with.
    pub(in crate::daemon) fn kill(&self) {
        for _ in 0..KILL_ATTEMPTS {
            if !self.refresh_now() {
                // After a normal stop the group of the leader is empty already, and nothing is killed or said.
                if let Some(leader) = self.leader()
                    && group_exists(leader)
                {
                    if self.leader_is_ours() {
                        eprintln!("cannot read the process list, only the process group of the leader is killed");
                        kill_group(leader);
                    } else {
                        eprintln!("cannot read the process list nor tell that the leader is still the process it was");
                    }
                }
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

/// Sends `SIGKILL` to the whole tree from a blocking thread, see [`Tree::kill`].
pub(in crate::daemon) async fn kill_tree(tree: &Tree) {
    let tree = tree.clone();
    let _ = tokio::task::spawn_blocking(move || tree.kill()).await;
}

/// Returns once no process of `tree` runs. Between 2 readings of the system, which cost a full listing of the processes,
/// the groups of the tree known are probed one by one.
async fn gone(tree: &Tree) {
    let mut since = Instant::now();
    loop {
        let looked = tree.clone();
        let looked = tokio::task::spawn_blocking(move || {
            let done = looked.is_gone(&mut since);
            (done, since)
        })
        .await;
        // The runtime is shutting down, there is nobody left to wait for the tree.
        let Ok((done, taken)) = looked else { return };
        since = taken;
        if done {
            return;
        }
        tokio::time::sleep(GONE_POLL).await;
    }
}

/// Asks the groups of the tree that the leader does not lead to stop, from a blocking thread.
async fn terminate_others(tree: &Tree) {
    let tree = tree.clone();
    let _ = tokio::task::spawn_blocking(move || tree.signal_other_groups(Signal::SIGTERM)).await;
}

/// Stops the tree of a Service whose leader is a child of the Daemon: `SIGTERM` to the group of the leader, then, once
/// the leader is gone, to the groups of the rest of the tree, which a tool such as turbo keeps its tasks in. It waits
/// until the whole tree is gone, and sends `SIGKILL` to every group of the tree that still has a process once the stop
/// timeout has passed.
///
/// The leader ending is not the end of the tree: turbo outlives the `bun run dev` that started it by a few tens of
/// milliseconds, the time it needs to stop the tasks.
pub(in crate::daemon) async fn stop_tree(child: &mut dyn ChildWrapper, tree: &Tree) {
    if let Err(error) = child.signal(Signal::SIGTERM as i32) {
        eprintln!("cannot send SIGTERM to process group {:?}: {error}", child.id());
    }
    let drained = tokio::time::timeout(STOP_TIMEOUT, async {
        let _ = child.wait().await;
        terminate_others(tree).await;
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
pub(in crate::daemon) async fn stop_detached(tree: &Tree) {
    let leader_group = tree.clone();
    let _ = tokio::task::spawn_blocking(move || leader_group.signal_leader_group(Signal::SIGTERM)).await;
    let drained = tokio::time::timeout(STOP_TIMEOUT, async {
        while tree.leader().is_some_and(is_alive) {
            tokio::time::sleep(GONE_POLL).await;
        }
        terminate_others(tree).await;
        gone(tree).await;
    })
    .await;
    if drained.is_err() {
        eprintln!("the tree of a service did not stop within {STOP_TIMEOUT:?}, it is killed");
    }
    kill_tree(tree).await;
}

/// The processes of the system, as of the last reading if it is younger than `age`.
fn listing(age: Duration) -> Option<Arc<Vec<Member>>> {
    static LAST: Mutex<Option<(Instant, Arc<Vec<Member>>)>> = Mutex::new(None);
    // Held while the system is read, so that a Service asking while another reads waits for that reading.
    let mut last = LAST
        .lock()
        .expect("the listing mutex is never poisoned, no code panics while holding it");
    if let Some((taken, listing)) = last.as_ref()
        && taken.elapsed() < age
    {
        return Some(Arc::clone(listing));
    }
    let listing = Arc::new(read_listing_or_refuse()?);
    *last = Some((Instant::now(), Arc::clone(&listing)));
    Some(listing)
}

/// Names a directory where tests watch and steer the listing of the processes, on every platform: a line is appended to
/// its file `calls` for each reading, and while its file `refuse` exists the reading fails, as it does when the system
/// refuses it. Only tests set it.
const TEST_LISTING_ENV: &str = "PROCLY_TEST_LISTING";

/// Reads every process of the system, unless a test refuses it, see [`TEST_LISTING_ENV`].
fn read_listing_or_refuse() -> Option<Vec<Member>> {
    if let Some(dir) = std::env::var_os(TEST_LISTING_ENV).map(std::path::PathBuf::from) {
        use std::io::Write as _;
        if let Ok(mut calls) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("calls"))
        {
            let _ = writeln!(calls, "listing");
        }
        if dir.join("refuse").exists() {
            return None;
        }
    }
    read_listing()
}

/// Reads every process of the system. macOS has no session column in `ps`, which prints 0, so `getsid` asks each.
#[cfg(not(target_os = "linux"))]
fn read_listing() -> Option<Vec<Member>> {
    let output = std::process::Command::new("ps")
        .args(["-axo", "pid=,pgid=,stat=,lstart="])
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .output()
        .ok()
        .filter(|output| output.status.success())?;
    Some(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(parse_member)
            // A process that ended since `ps` read it has no session any more. The group is read with the session, from
            // the same process, so that a pid given to another process since `ps` read it never pairs a group with a
            // session that are not its own.
            .filter_map(|mut member| {
                member.sid = nix::unistd::getsid(Some(member.pid)).ok()?;
                member.pgid = nix::unistd::getpgid(Some(member.pid)).ok()?;
                Some(member)
            })
            .collect(),
    )
}

/// See the other version above: the files of `/proc` hold the session, and the start as clock ticks after boot.
#[cfg(target_os = "linux")]
fn read_listing() -> Option<Vec<Member>> {
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
/// Reads a line of `ps -axo pid=,pgid=,stat=,lstart=`, such as `4242 4242 Ss Sun Oct 18 20:52:01 2026`. The session is
/// not in it, it is `4242` here until the caller asks the system.
fn parse_member(line: &str) -> Option<Member> {
    let mut fields = line.split_whitespace();
    let mut number = || fields.next()?.parse().ok().map(Pid::from_raw);
    let [pid, group] = [number()?, number()?];
    let stat = fields.next()?;
    let started = fields.collect::<Vec<_>>().join(" ");
    if started.is_empty() {
        return None;
    }
    Some(Member {
        pid,
        pgid: group,
        sid: pid,
        started,
        zombie: stat.starts_with('Z'),
    })
}

#[cfg(any(target_os = "linux", test))]
/// Reads the state, the process group, the session and the start of `/proc/<pid>/stat`, the 3rd, 5th, 6th and 22nd
/// fields, counted after the name of the command like [`super::parse_proc_stat`] does.
fn parse_proc_member(pid: i32, stat: &str) -> Option<Member> {
    let (_, fields) = stat.rsplit_once(')')?;
    let fields: Vec<&str> = fields.split_whitespace().collect();
    Some(Member {
        pid: Pid::from_raw(pid),
        pgid: Pid::from_raw(fields.get(2)?.parse().ok()?),
        sid: Pid::from_raw(fields.get(3)?.parse().ok()?),
        started: format!("{} ticks after boot", fields.get(19)?.parse::<u64>().ok()?),
        zombie: fields.first()?.starts_with('Z'),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(pid: i32, group: i32, session: i32, started: &str) -> Member {
        Member {
            pid: Pid::from_raw(pid),
            pgid: Pid::from_raw(group),
            sid: Pid::from_raw(session),
            started: started.to_owned(),
            zombie: false,
        }
    }

    /// The tree of the session led by 100, which the Daemon started and whose Daemon runs in the group 7, seen in
    /// `listing`.
    fn session_100(listing: &[Member]) -> Known {
        let mut known = Known::spawned(Pid::from_raw(100));
        known.absorb(listing, Pid::from_raw(7));
        known
    }

    /// The tree of the leader 100 that a previous Daemon recorded with the start `t0`, seen in `listing`.
    fn recorded_100(listing: Option<&[Member]>) -> Known {
        Known::recorded(Pid::from_raw(100), Some("t0"), listing, Pid::from_raw(7))
    }

    fn absorb(known: &mut Known, listing: &[Member]) {
        known.absorb(listing, Pid::from_raw(7));
    }

    fn groups(pgids: &[i32]) -> BTreeSet<Pid> {
        pgids.iter().copied().map(Pid::from_raw).collect()
    }

    #[test]
    fn a_task_in_a_group_of_its_own_is_found_through_its_session() {
        // 100 leads the session, 101 is turbo in its group, 102 and 103 are tasks that lead groups of their own.
        let known = session_100(&[
            member(100, 100, 100, "t0"),
            member(101, 100, 100, "t1"),
            member(102, 102, 100, "t2"),
            member(103, 102, 100, "t3"),
            member(900, 900, 900, "t9"),
        ]);

        assert_eq!(known.groups, groups(&[100, 102]));
    }

    #[test]
    fn an_orphan_adopted_by_init_stays_in_the_tree_whoever_its_parent_was() {
        // Nothing here says who the parent of 102 is, or that its parent ended: its session says all there is to know.
        let known = session_100(&[member(102, 102, 100, "t2")]);

        assert_eq!(known.groups, groups(&[102]));
    }

    #[test]
    fn a_process_of_the_daemons_group_never_enters_the_tree() {
        let known = session_100(&[member(100, 100, 100, "t0"), member(105, 7, 100, "t5")]);

        assert_eq!(known.groups, groups(&[100]));
    }

    #[test]
    fn a_process_that_leaves_the_session_leaves_the_tree() {
        let known = session_100(&[member(100, 100, 100, "t0"), member(102, 102, 102, "t2")]);

        assert_eq!(known.groups, groups(&[100]));
    }

    #[test]
    fn a_reused_pid_is_not_in_the_tree_and_its_group_is_not_signalled() {
        let mut known = session_100(&[member(100, 100, 100, "t0"), member(102, 102, 100, "t2")]);

        // 102 ended and an unrelated process got its pid, in a session and a group of its own.
        known.absorb(
            &[member(100, 100, 100, "t0"), member(102, 102, 102, "later")],
            Pid::from_raw(7),
        );

        assert_eq!(known.groups, groups(&[100]));
    }

    #[test]
    fn a_stranger_that_got_the_pid_of_the_leader_and_leads_a_session_takes_nobody_along() {
        let mut known = session_100(&[member(100, 100, 100, "t0"), member(101, 100, 100, "t1")]);

        // 100 ended and a process that got its pid leads a session, with a child of its own: not the Service's.
        known.absorb(
            &[member(100, 100, 100, "other"), member(110, 100, 100, "other2")],
            Pid::from_raw(7),
        );

        assert_eq!(known.groups, BTreeSet::new());
    }

    #[test]
    fn a_stranger_that_got_the_pid_of_the_leader_without_a_session_leaves_the_rest_alone() {
        let mut known = session_100(&[member(100, 100, 100, "t0"), member(102, 102, 100, "t2")]);

        // The leader ended, a process that got its pid never called `setsid`: the task of the Service is still there.
        known.absorb(
            &[member(100, 55, 55, "other"), member(102, 102, 100, "t2")],
            Pid::from_raw(7),
        );

        assert_eq!(known.groups, groups(&[102]));
    }

    #[test]
    fn a_zombie_is_not_a_process_that_runs() {
        let mut zombie = member(103, 103, 100, "t3");
        zombie.zombie = true;

        let known = session_100(&[member(100, 100, 100, "t0"), zombie]);

        assert_eq!(known.groups, groups(&[100]));
    }

    #[test]
    fn a_service_an_older_daemon_started_is_known_by_its_group() {
        let mut known = Known {
            scope: Some(Scope::Group(Pid::from_raw(100))),
            ..Known::default()
        };

        // 100 never led a session, its group holds it and a sibling, and a task in a group of its own is none of theirs.
        known.absorb(
            &[
                member(100, 100, 7, "t0"),
                member(101, 100, 7, "t1"),
                member(102, 102, 7, "t2"),
            ],
            Pid::from_raw(7),
        );

        assert_eq!(known.groups, groups(&[100]));
    }

    #[test]
    fn a_leader_that_does_not_show_its_start_yet_is_given_the_one_of_its_first_listing() {
        let mut known = Known {
            scope: Some(Scope::Session(Pid::from_raw(100))),
            ..Known::default()
        };

        known.absorb(&[member(100, 100, 100, "t0")], Pid::from_raw(7));
        assert_eq!(known.leader_started.as_deref(), Some("t0"));
        known.absorb(&[member(100, 100, 100, "other")], Pid::from_raw(7));

        assert_eq!(known.groups, BTreeSet::new());
    }

    #[test]
    fn a_recorded_leader_that_a_stranger_holds_the_pid_of_is_not_adopted_with_its_children() {
        // The record names the start t0. When the Daemon looks, a stranger that leads a session holds the pid.
        let mut known = Known {
            scope: Some(Scope::Session(Pid::from_raw(100))),
            leader_started: Some("t0".to_owned()),
            ..Known::default()
        };

        known.absorb(
            &[member(100, 100, 100, "stranger"), member(101, 100, 100, "its child")],
            Pid::from_raw(7),
        );

        assert_eq!(known.groups, BTreeSet::new());
    }

    #[test]
    fn a_recorded_leader_found_running_while_the_processes_cannot_be_listed_keeps_its_process_group() {
        let known = Known::recorded(Pid::from_raw(100), Some("t0"), None, Pid::from_raw(7));

        assert_eq!(known.groups, groups(&[100]));
        assert_eq!(known.leader_started.as_deref(), Some("t0"));
    }

    #[test]
    fn a_listing_that_comes_later_tells_a_recorded_leader_that_leads_a_session_from_one_that_does_not() {
        let mut session = Known::recorded(Pid::from_raw(100), Some("t0"), None, Pid::from_raw(7));
        let mut older = Known::recorded(Pid::from_raw(100), Some("t0"), None, Pid::from_raw(7));

        session.absorb(
            &[member(100, 100, 100, "t0"), member(102, 102, 100, "t2")],
            Pid::from_raw(7),
        );
        older.absorb(
            &[member(100, 100, 7, "t0"), member(102, 102, 7, "t2")],
            Pid::from_raw(7),
        );

        assert_eq!(session.groups, groups(&[100, 102]));
        assert_eq!(older.groups, groups(&[100]));
    }

    #[test]
    fn a_recorded_leader_never_seen_running_takes_nothing_of_its_session_or_group_into_the_tree() {
        let mut known = recorded_100(None);

        // 100 is gone before any listing. A process leads a session of its own that has the id 100 and a task, and
        // another sits in the group 100: nothing shows that either has been there since the Service, so both are left alone.
        absorb(
            &mut known,
            &[
                member(150, 150, 100, "t2"),
                member(151, 100, 7, "t3"),
                member(900, 900, 900, "t9"),
            ],
        );
        assert_eq!(known.groups, BTreeSet::new());

        // And whatever comes of that session later, it is not the tree's either.
        absorb(&mut known, &[member(150, 150, 100, "t2"), member(152, 152, 100, "t4")]);
        assert_eq!(known.groups, BTreeSet::new());
    }

    #[test]
    fn a_session_emptied_and_taken_by_a_stranger_after_the_leader_ended_is_not_adopted() {
        // The Service was seen running: the leader 100, the server 101 in a group of its own.
        let mut known = recorded_100(Some(&[member(100, 100, 100, "t0"), member(101, 101, 100, "t1")]));
        assert_eq!(known.groups, groups(&[100, 101]));

        // Both ended, and a stranger got the pid 100, led a session of its own and started 150, then ended itself.
        absorb(
            &mut known,
            &[member(100, 100, 100, "stranger"), member(150, 150, 100, "its child")],
        );
        assert_eq!(known.groups, BTreeSet::new());
        absorb(&mut known, &[member(150, 150, 100, "its child")]);
        assert_eq!(known.groups, BTreeSet::new());
    }

    #[test]
    fn a_session_emptied_and_taken_by_a_stranger_between_2_listings_is_not_adopted_even_when_the_leader_is_not_listed()
    {
        let mut known = recorded_100(Some(&[member(100, 100, 100, "t0"), member(101, 101, 100, "t1")]));

        // The listing that follows finds no leader, and in the session the child of a stranger, which was not known.
        absorb(&mut known, &[member(150, 150, 100, "its child")]);

        assert_eq!(known.groups, BTreeSet::new());
    }

    #[test]
    fn a_stranger_that_holds_the_group_of_a_leader_that_ended_is_not_adopted() {
        // A Service an older Daemon started: the leader 100 and its sibling 101 in the group 100, no session of its own.
        let mut known = recorded_100(Some(&[member(100, 100, 7, "t0"), member(101, 100, 7, "t1")]));
        assert_eq!(known.scope, Some(Scope::Group(Pid::from_raw(100))));

        absorb(&mut known, &[member(160, 100, 55, "stranger")]);

        assert_eq!(known.groups, BTreeSet::new());
    }

    #[test]
    fn what_the_tree_knew_stays_in_it_when_the_leader_ends_and_what_that_started_later_follows() {
        let mut known = recorded_100(Some(&[member(100, 100, 100, "t0"), member(101, 101, 100, "t1")]));

        // The leader ended, 101 lives on and has started 102 since the last listing: the session never emptied.
        absorb(&mut known, &[member(101, 101, 100, "t1"), member(102, 102, 100, "t2")]);
        assert_eq!(known.groups, groups(&[101, 102]));

        // 101 ends, 102 was in the last listing and is still the tree's.
        absorb(&mut known, &[member(102, 102, 100, "t2"), member(103, 103, 100, "t3")]);
        assert_eq!(known.groups, groups(&[102, 103]));

        // Everything known ended: the session is no one's to adopt any more, whatever it holds.
        absorb(
            &mut known,
            &[member(103, 103, 100, "t3-reused"), member(104, 104, 100, "t4")],
        );
        assert_eq!(known.groups, BTreeSet::new());
        absorb(&mut known, &[member(104, 104, 100, "t4")]);
        assert_eq!(known.groups, BTreeSet::new());
    }

    #[test]
    fn a_zombie_leader_not_reaped_yet_still_holds_its_session_for_the_tree() {
        let mut leader = member(100, 100, 100, "t0");
        leader.zombie = true;
        let mut known = recorded_100(None);

        absorb(&mut known, &[leader, member(102, 102, 100, "t2")]);

        assert_eq!(known.scope, Some(Scope::Session(Pid::from_raw(100))));
        assert_eq!(known.groups, groups(&[102]));
    }

    #[test]
    fn a_leader_the_daemon_reaped_hands_its_session_to_the_first_listing_only() {
        let mut known = Known::spawned(Pid::from_raw(100));

        // The Daemon reaped the leader: 102 is a task it started in a group of its own, not seen before.
        absorb(&mut known, &[member(102, 102, 100, "t2")]);
        assert_eq!(known.groups, groups(&[102]));

        // Later, 102 is gone and another process leads a session with the same id: not the tree's.
        absorb(&mut known, &[member(150, 150, 100, "stranger's child")]);
        assert_eq!(known.groups, BTreeSet::new());
    }

    #[test]
    fn a_recorded_tree_is_unlisted_until_a_listing_is_read_even_when_that_listing_does_not_find_the_leader() {
        let recorded = |listing: Option<&[Member]>| Tree {
            known: Arc::new(Mutex::new(Known::recorded(
                Pid::from_raw(100),
                Some("t0"),
                listing,
                Pid::from_raw(7),
            ))),
        };
        let stranger = [member(100, 100, 100, "stranger")];

        assert!(recorded(None).is_unlisted());
        assert!(!recorded(Some(&[member(100, 100, 100, "t0")])).is_unlisted());
        // A stranger holds the pid, nothing in the listing is the Service's, and it is no longer a doubt.
        assert!(!recorded(Some(&stranger)).is_unlisted());
        assert!(!recorded(Some(&[])).is_unlisted());
    }

    #[test]
    fn a_recorded_leader_whose_pid_a_stranger_holds_is_not_adopted_whether_or_not_the_processes_were_listed() {
        let stranger = [member(100, 100, 100, "stranger"), member(101, 100, 100, "its child")];

        let known = Known::recorded(Pid::from_raw(100), Some("t0"), Some(&stranger), Pid::from_raw(7));

        assert_eq!(known.groups, BTreeSet::new());
    }

    #[test]
    fn a_leader_start_given_at_launch_keeps_a_stranger_that_got_its_pid_before_the_first_listing_out_of_the_tree() {
        let tree = Tree::default();
        tree.begin(Pid::from_raw(100));
        tree.note_start(Some("t0".to_owned()));

        // The leader was reaped and a stranger that leads a session got its pid, before anything listed the processes.
        tree.known().absorb(
            &[member(100, 100, 100, "other"), member(110, 100, 100, "other2")],
            Pid::from_raw(7),
        );

        assert_eq!(tree.groups(), BTreeSet::new());
    }

    #[test]
    fn a_leader_is_ours_without_a_listing_only_if_it_has_the_start_the_tree_saw_or_has_ended() {
        use std::os::unix::process::CommandExt as _;

        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .expect("start a process that leads a group");
        let leader = Pid::from_raw(i32::try_from(child.id()).expect("a pid fits in an i32"));
        let started = super::super::identity(leader).expect("the process runs").started;
        let tree = Tree::default();
        tree.begin(leader);

        // Nothing says which process this is: whoever leads the group could be a stranger that got the pid.
        let unknown = tree.leader_is_ours();
        tree.note_start(Some("another start".to_owned()));
        let another = tree.leader_is_ours();
        let tree_of_the_process = Tree::default();
        tree_of_the_process.begin(leader);
        tree_of_the_process.note_start(Some(started));
        let same = tree_of_the_process.leader_is_ours();
        child.kill().expect("stop the process");
        child.wait().expect("reap the process");

        assert!(!unknown, "a leader whose start was never seen was taken for the tree's");
        assert!(!another, "a leader with another start was taken for the tree's");
        assert!(same, "the leader of the tree was not recognized");
        // Gone, and its group empty: nothing left to signal, and nobody else's to spare.
        assert!(tree_of_the_process.leader_is_ours());
    }

    #[test]
    fn the_start_of_a_leader_is_noted_once() {
        let tree = Tree::default();
        tree.begin(Pid::from_raw(100));

        tree.note_start(Some("t0".to_owned()));
        tree.note_start(Some("later".to_owned()));
        tree.note_start(None);

        assert_eq!(tree.known().leader_started.as_deref(), Some("t0"));
    }

    #[test]
    fn reads_a_process_of_the_ps_listing_with_its_start_and_whether_it_ended() {
        assert_eq!(
            parse_member(" 4242  4240 Ss   Sun Oct 18 20:52:01 2026\n"),
            Some(member(4242, 4240, 4242, "Sun Oct 18 20:52:01 2026"))
        );
        assert!(parse_member("4242 4240 Z Sun Oct 18 20:52:01 2026").is_some_and(|member| member.zombie));
        assert!(parse_member("4242 4240 Ss").is_none());
        assert!(parse_member("pid pgid stat started").is_none());
    }

    #[test]
    fn reads_a_process_of_proc_with_its_group_session_and_start() {
        let stat = "4242 (my (odd) cmd) S 1 4240 4239 0 -1 4194560 120 0 0 0 1 2 0 0 20 0 1 0 987654 1234 56 \
                    18446744073709551615\n";
        assert_eq!(
            parse_proc_member(4242, stat),
            Some(member(4242, 4240, 4239, "987654 ticks after boot"))
        );
        assert!(parse_proc_member(4242, &stat.replace(") S ", ") Z ")).is_some_and(|member| member.zombie));
        assert_eq!(parse_proc_member(4242, "4242 (cmd) S 1"), None);
    }

    #[test]
    fn the_start_of_a_process_reads_the_same_in_the_listing_and_in_the_identity_whatever_the_day_of_the_month() {
        // `ps` pads a day under 10 with a second space, the 2 readings must still compare equal.
        let listed = parse_member("4242 4242 Ss Mon Nov  9 09:42:20 2026").map(|member| member.started);
        let identified = super::super::parse_identity("4242 Mon Nov  9 09:42:20 2026").map(|identity| identity.started);

        assert_eq!(listed, identified);
    }
}
