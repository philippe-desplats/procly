//! The tree of a Service: every process that started from its leader and did not leave its session.
//!
//! A Service leads a session of its own, so everything it starts belongs to that session, whatever process group a
//! tool puts its tasks in (turbo runs each in one of its own) and whoever adopts them once their parent ended. Finding
//! the tree is a matter of reading the session of each process, never of following parents, so that a task whose
//! parent ended a few milliseconds after starting it is not missed. A process that calls `setsid` leaves the tree,
//! which is what a daemon meant to outlive the Service does.
//!
//! A Service that an older Daemon started leads a process group only, and is known by that group.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use nix::sys::signal::Signal;
use process_wrap::tokio::ChildWrapper;

use super::{GONE_POLL, KILL_ATTEMPTS, Pid, STOP_TIMEOUT, is_alive, kill_group, normalized, own_group, signal_group};

/// How long a listing of the processes is shared by the Services that ask for one, so that N Services looking at
/// their tree within the same few milliseconds cost one listing.
const SHARED_FOR: Duration = Duration::from_millis(40);

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
}

impl Scope {
    fn leader(self) -> Pid {
        match self {
            Self::Session(leader) | Self::Group(leader) => leader,
        }
    }

    fn holds(self, member: &Member) -> bool {
        match self {
            Self::Session(leader) => member.sid == leader,
            Self::Group(leader) => member.pgid == leader,
        }
    }
}

#[derive(Debug, Default)]
struct Known {
    scope: Option<Scope>,
    /// When the leader started, once known. It tells the leader from a process that got its pid since.
    leader_started: Option<String>,
    /// The process groups of the processes seen alive in the last listing.
    groups: BTreeSet<Pid>,
}

impl Known {
    /// Takes a listing of the system in: the groups of the processes of the tree, now.
    fn absorb(&mut self, listing: &[Member], daemon_group: Pid) {
        let Some(scope) = self.scope else { return };
        let leader = listing
            .iter()
            .find(|member| member.pid == scope.leader() && !member.zombie && scope.holds(member));
        match (leader, &self.leader_started) {
            (Some(leader), None) => self.leader_started = Some(leader.started.clone()),
            // The pid of the leader leads a session or a group again, some other process got it: whatever else holds
            // that id is that process's, not the Service's.
            (Some(leader), Some(started)) if leader.started != *started => {
                self.groups.clear();
                return;
            }
            _ => {}
        }
        self.groups = listing
            .iter()
            .filter(|member| !member.zombie && scope.holds(member) && member.pgid != daemon_group)
            .map(|member| member.pgid)
            .collect();
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
        *self.known() = Known {
            scope: Some(Scope::Session(leader)),
            leader_started: None,
            groups: BTreeSet::from([leader]),
        };
    }

    /// Starts the tree over with the leader `leader` that a previous Daemon started and recorded with the start
    /// `started`, if it is still the process that leader was. An empty tree otherwise. Blocking.
    pub(in crate::daemon) fn begin_recorded(&self, leader: Pid, started: Option<&str>) {
        let started = started.map(normalized);
        let known = listing(Duration::ZERO)
            .and_then(|listing| {
                let row = listing.iter().find(|member| member.pid == leader && !member.zombie)?;
                if started.as_ref().is_some_and(|started| *started != row.started) {
                    return None;
                }
                // A leader that does not lead its own session was started before Services did.
                let scope = if row.sid == leader {
                    Scope::Session(leader)
                } else {
                    Scope::Group(leader)
                };
                let mut known = Known {
                    scope: Some(scope),
                    leader_started: Some(row.started.clone()),
                    groups: BTreeSet::new(),
                };
                known.absorb(&listing, own_group());
                Some(known)
            })
            .unwrap_or_default();
        *self.known() = known;
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

    /// Whether no process of the tree was seen alive in the last listing.
    fn is_empty(&self) -> bool {
        self.known().groups.is_empty()
    }

    fn leader(&self) -> Option<Pid> {
        self.known().scope.map(Scope::leader)
    }

    /// Sends `signal` to the group of the leader, if a listing taken now shows a process in it. Blocking.
    fn signal_leader_group(&self, signal: Signal) {
        if let Some(leader) = self.leader()
            && self.refresh_now()
            && self.groups().contains(&leader)
        {
            signal_group(leader, signal);
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
    /// except the group of the leader: it keeps its id reserved while any of its members lives.
    pub(in crate::daemon) fn kill(&self) {
        for _ in 0..KILL_ATTEMPTS {
            if !self.refresh_now() {
                eprintln!("cannot read the process list, only the process group of the leader is killed");
                if let Some(leader) = self.leader() {
                    kill_group(leader);
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

/// Returns once no process of `tree` runs.
async fn gone(tree: &Tree) {
    loop {
        let looked = tree.clone();
        let _ = tokio::task::spawn_blocking(move || looked.refresh_now()).await;
        if tree.is_empty() {
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
    let listing = Arc::new(read_listing()?);
    *last = Some((Instant::now(), Arc::clone(&listing)));
    Some(listing)
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
            // A process that ended since `ps` read it has no session any more.
            .filter_map(|mut member| {
                member.sid = nix::unistd::getsid(Some(member.pid)).ok()?;
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

    /// The tree of the session led by 100, whose Daemon runs in the group 7, seen in `listing`.
    fn session_100(listing: &[Member]) -> Known {
        let mut known = Known {
            scope: Some(Scope::Session(Pid::from_raw(100))),
            ..Known::default()
        };
        known.absorb(listing, Pid::from_raw(7));
        known
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
