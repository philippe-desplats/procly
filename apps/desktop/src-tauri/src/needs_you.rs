//! "Needs you": the one thing the app derives from the `Status` answer instead of drawing it (`docs/rules/tauri.md`).
//!
//! It lists the failures the developer has not marked as Seen and the Foreign processes, nothing routine. The Seen
//! set is held here too, so that the window and the menu bar menu read the same list. It is presentation, it decides
//! nothing in the Daemon.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::{fs, io};

use procly_client::protocol::{
    ForeignProcess, JobEnd, JobState, JobStatus, Response, ServiceEnd, ServiceState, StatusReport,
};
use serde::{Deserialize, Serialize, Serializer};

use crate::atomic;
use crate::poller::{Answer, Update};

/// What identifies a failure, so that one that happens again is not taken for the one already seen.
///
/// A Service is keyed by its Worktree, its name and when it last started: a Service that starts again and fails
/// comes back. A Job has an `id` of its own.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum FailureKey {
    Service {
        worktree: PathBuf,
        service: String,
        started_at: u64,
    },
    Job {
        id: String,
    },
}

/// One line of "Needs you", with what the window needs to draw it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum NeedsYou {
    /// A Service that ended in a way that is not routine.
    Service {
        worktree: PathBuf,
        service: String,
        started_at: u64,
        end: ServiceEnd,
    },
    /// A Job that ended in a way that is not routine. `worktree` is empty for a Job started outside any Repository.
    Job {
        worktree: Option<PathBuf>,
        id: String,
        command: String,
        end: JobEnd,
    },
    /// A process or a container that runs in a Worktree without Procly having started it.
    Foreign { worktree: PathBuf, process: ForeignProcess },
}

/// Whether a Service ended in a way that needs the developer.
///
/// Only `stopped`, `idle` and `exited` with code 0 are routine. Every other end counts as a failure, so a kind this
/// app does not know yet is one too.
fn service_end_is_failure(end: &ServiceEnd) -> bool {
    !matches!(
        end,
        ServiceEnd::Stopped | ServiceEnd::Idle { .. } | ServiceEnd::Exited { code: 0 }
    )
}

/// Whether a Job ended in a way that needs the developer: any end but `exited` with code 0.
fn job_end_is_failure(end: &JobEnd) -> bool {
    !matches!(end, JobEnd::Exited { code: 0 })
}

/// Every failure of the answer with its key, in the order of the answer.
fn failures(report: &StatusReport) -> Vec<(FailureKey, NeedsYou)> {
    let mut found = Vec::new();
    for worktree in report.repositories.iter().flat_map(|repository| &repository.worktrees) {
        for service in &worktree.services {
            if let ServiceState::Ended { end } = service.state
                && service_end_is_failure(&end)
            {
                found.push((
                    FailureKey::Service {
                        worktree: worktree.worktree.path.clone(),
                        service: service.service.clone(),
                        started_at: service.started_at,
                    },
                    NeedsYou::Service {
                        worktree: worktree.worktree.path.clone(),
                        service: service.service.clone(),
                        started_at: service.started_at,
                        end,
                    },
                ));
            }
        }
        for job in &worktree.jobs {
            found.extend(job_failure(job, Some(&worktree.worktree.path)));
        }
    }
    for job in &report.jobs_outside_repositories {
        found.extend(job_failure(job, None));
    }
    found
}

fn job_failure(job: &JobStatus, worktree: Option<&Path>) -> Option<(FailureKey, NeedsYou)> {
    let JobState::Ended { end } = job.state else {
        return None;
    };
    job_end_is_failure(&end).then(|| {
        (
            FailureKey::Job { id: job.id.clone() },
            NeedsYou::Job {
                worktree: worktree.map(Path::to_path_buf),
                id: job.id.clone(),
                command: job.command.clone(),
                end,
            },
        )
    })
}

/// The Foreign processes of every Worktree of the answer.
fn foreign_processes(report: &StatusReport) -> impl Iterator<Item = NeedsYou> {
    report
        .repositories
        .iter()
        .flat_map(|repository| &repository.worktrees)
        .flat_map(|worktree| {
            worktree.foreign.iter().map(|process| NeedsYou::Foreign {
                worktree: worktree.worktree.path.clone(),
                process: process.clone(),
            })
        })
}

/// What needs the developer in `report`: the failures not marked as Seen, then the Foreign processes.
pub(crate) fn needs_you(report: &StatusReport, seen: &Seen) -> Vec<NeedsYou> {
    failures(report)
        .into_iter()
        .filter(|(key, _)| !seen.contains(key))
        .map(|(_, line)| line)
        .chain(foreign_processes(report))
        .collect()
}

/// The failures the developer marked as Seen. A key leaves the set once the answer no longer holds its failure.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Seen {
    #[serde(default)]
    seen: BTreeSet<FailureKey>,
}

impl Seen {
    fn contains(&self, key: &FailureKey) -> bool {
        self.seen.contains(key)
    }

    /// Marks the failure `key` as Seen when the answer holds it, and tells whether the set changed. A key the answer
    /// does not hold is not a failure the developer saw.
    fn mark(&mut self, key: FailureKey, report: &StatusReport) -> bool {
        failures(report).iter().any(|(held, _)| *held == key) && self.seen.insert(key)
    }

    /// Drops every key the answer does not hold a failure for, and tells whether the set changed.
    fn prune(&mut self, report: &StatusReport) -> bool {
        let held: BTreeSet<FailureKey> = failures(report).into_iter().map(|(key, _)| key).collect();
        let before = self.seen.len();
        self.seen.retain(|key| held.contains(key));
        self.seen.len() != before
    }

    fn keys(&self) -> Vec<FailureKey> {
        self.seen.iter().cloned().collect()
    }

    /// The set kept in `path`. A file that is missing, unreadable or not what this app wrote gives an empty set:
    /// the worst that costs is a failure showing again.
    fn load(path: &Path) -> Self {
        fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Writes the set to `path` so that a reader sees the old file or the new one, never half of it.
    fn save(&self, path: &Path) -> Result<(), SeenError> {
        let write = |source| SeenError {
            path: path.to_path_buf(),
            source,
        };
        let bytes = serde_json::to_vec_pretty(self).map_err(|error| write(io::Error::other(error)))?;
        atomic::write(path, &bytes).map_err(write)
    }
}

/// The Seen set could not be written, or the thread that writes it broke. The mark holds until the app quits.
#[derive(Debug, thiserror::Error)]
#[error("cannot save the Seen failures to {}: {source}", path.display())]
pub(crate) struct SeenError {
    path: PathBuf,
    #[source]
    source: io::Error,
}

impl SeenError {
    /// The thread that writes the set stopped before it could.
    pub(crate) fn task(error: &tauri::Error) -> Self {
        Self {
            path: PathBuf::new(),
            source: io::Error::other(error.to_string()),
        }
    }
}

impl Serialize for SeenError {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Failure {
            kind: &'static str,
            message: String,
        }
        Failure {
            kind: "seen",
            message: self.to_string(),
        }
        .serialize(serializer)
    }
}

/// An update of the poller with what the window needs next to it: "Needs you" and the Seen set. The `Response` is the
/// one the Daemon wrote.
#[derive(Debug, Serialize)]
pub(crate) struct Presented {
    #[serde(flatten)]
    update: Arc<Update>,
    needs_you: Vec<NeedsYou>,
    seen: Vec<FailureKey>,
}

/// The Seen set as the rest of the app holds it, with the file it survives in.
#[derive(Debug, Clone)]
pub(crate) struct Attention {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    path: PathBuf,
    seen: Mutex<Seen>,
}

fn report_of(update: &Update) -> Option<&StatusReport> {
    match &update.answer {
        Answer::Response(Response::Status(report)) => Some(report),
        Answer::Response(_) | Answer::Failure(_) => None,
    }
}

impl Attention {
    /// Reads the Seen set kept in `path`, which may not exist yet.
    pub(crate) fn open(path: PathBuf) -> Self {
        let seen = Seen::load(&path);
        Self {
            inner: Arc::new(Inner {
                path,
                seen: Mutex::new(seen),
            }),
        }
    }

    fn seen(&self) -> std::sync::MutexGuard<'_, Seen> {
        // Nothing here panics while holding the lock, and a set that was half changed is still a valid set.
        self.inner.seen.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn present(update: Arc<Update>, seen: &Seen) -> Presented {
        let needs_you = report_of(&update).map_or_else(Vec::new, |report| needs_you(report, seen));
        Presented {
            update,
            needs_you,
            seen: seen.keys(),
        }
    }

    /// `update` as the window receives it, without changing the Seen set.
    pub(crate) fn present_update(&self, update: Arc<Update>) -> Presented {
        Self::present(update, &self.seen())
    }

    /// A new answer of the Daemon: the Seen set forgets what the answer no longer holds, then `update` is presented.
    /// Only an answer to `Status` prunes, so that a Daemon that stops does not make the developer see everything
    /// again.
    pub(crate) fn on_answer(&self, update: Arc<Update>) -> (Presented, Result<(), SeenError>) {
        let mut seen = self.seen();
        let saved = match report_of(&update) {
            Some(report) if seen.prune(report) => seen.save(&self.inner.path),
            Some(_) | None => Ok(()),
        };
        (Self::present(update, &seen), saved)
    }

    /// The developer marked `key` as Seen. `last` is the answer on screen, which says whether `key` is a failure.
    pub(crate) fn mark(&self, key: FailureKey, last: Arc<Update>) -> (Presented, Result<(), SeenError>) {
        let mut seen = self.seen();
        let saved = match report_of(&last) {
            Some(report) if seen.mark(key, report) => seen.save(&self.inner.path),
            Some(_) | None => Ok(()),
        };
        (Self::present(last, &seen), saved)
    }
}

#[cfg(test)]
mod tests {
    use procly_client::protocol::{RepositoryStatus, ServiceStatus, Worktree, WorktreeStatus};

    use super::*;

    fn worktree(path: &str) -> WorktreeStatus {
        WorktreeStatus {
            worktree: Worktree {
                path: PathBuf::from(path),
                slug: "web".to_owned(),
                main: true,
            },
            branch: None,
            listed: true,
            services: Vec::new(),
            jobs: Vec::new(),
            foreign: Vec::new(),
            containers: Vec::new(),
        }
    }

    fn ended_service(name: &str, started_at: u64, end: ServiceEnd) -> ServiceStatus {
        ServiceStatus {
            service: name.to_owned(),
            port: None,
            reattached: false,
            started_at,
            restartable: false,
            state: ServiceState::Ended { end },
        }
    }

    fn job(id: &str, state: JobState) -> JobStatus {
        JobStatus {
            id: id.to_owned(),
            command: "make test".to_owned(),
            cwd: PathBuf::from("/p/work/acme/web"),
            state,
        }
    }

    fn report(worktrees: Vec<WorktreeStatus>) -> StatusReport {
        StatusReport {
            repositories: vec![RepositoryStatus {
                git_common_dir: PathBuf::from("/p/work/acme/web/.git"),
                worktrees,
            }],
            jobs_outside_repositories: Vec::new(),
        }
    }

    fn report_with_service(end: ServiceEnd) -> StatusReport {
        let mut web = worktree("/p/work/acme/web");
        web.services.push(ended_service("api", 1_000, end));
        report(vec![web])
    }

    fn needs(report: &StatusReport) -> Vec<NeedsYou> {
        needs_you(report, &Seen::default())
    }

    #[test]
    fn a_service_that_ended_without_a_failure_needs_nobody() {
        for end in [
            ServiceEnd::Stopped,
            ServiceEnd::Idle { timeout_ms: 600_000 },
            ServiceEnd::Exited { code: 0 },
        ] {
            assert_eq!(needs(&report_with_service(end)), [], "{end:?} is routine");
        }
    }

    #[test]
    fn every_other_end_of_a_service_is_a_failure() {
        let failures = [
            ServiceEnd::Exited { code: 1 },
            ServiceEnd::Exited { code: -1 },
            ServiceEnd::Signaled { signal: 9 },
            ServiceEnd::HealthTimeout { timeout_ms: 30_000 },
            ServiceEnd::Failed { restarts: 3 },
            ServiceEnd::PortMismatch {
                leased: 24_000,
                actual: 24_001,
            },
            ServiceEnd::Lost,
            ServiceEnd::Orphaned {
                pgid: 4_321,
                port: Some(24_000),
            },
            ServiceEnd::NotListening { port: 24_000 },
        ];
        for end in failures {
            assert_eq!(
                needs(&report_with_service(end)),
                [NeedsYou::Service {
                    worktree: PathBuf::from("/p/work/acme/web"),
                    service: "api".to_owned(),
                    started_at: 1_000,
                    end,
                }],
                "{end:?} is a failure"
            );
        }
    }

    #[test]
    fn a_kind_the_list_of_routine_ends_does_not_name_is_a_failure() {
        // The routine ends are an allowlist, so an end added to the protocol later is a failure until someone decides
        // it is not. These are the only 3 the rule spares.
        let routine = |end: ServiceEnd| !service_end_is_failure(&end);

        assert!(routine(ServiceEnd::Stopped));
        assert!(routine(ServiceEnd::Idle { timeout_ms: 1 }));
        assert!(routine(ServiceEnd::Exited { code: 0 }));
        assert!(!routine(ServiceEnd::Exited { code: 2 }));
    }

    #[test]
    fn a_service_that_still_runs_or_is_about_to_is_not_a_failure() {
        let mut web = worktree("/p/work/acme/web");
        for (name, state) in [
            ("starting", ServiceState::Starting),
            ("healthy", ServiceState::Healthy),
            ("restarting", ServiceState::Restarting),
        ] {
            web.services.push(ServiceStatus {
                service: name.to_owned(),
                port: None,
                reattached: false,
                started_at: 1,
                restartable: false,
                state,
            });
        }

        assert_eq!(needs(&report(vec![web])), []);
    }

    #[test]
    fn a_job_is_a_failure_unless_it_exited_with_code_0() {
        let mut web = worktree("/p/work/acme/web");
        web.jobs = vec![
            job("j-running", JobState::Running),
            job(
                "j-ok",
                JobState::Ended {
                    end: JobEnd::Exited { code: 0 },
                },
            ),
            job(
                "j-red",
                JobState::Ended {
                    end: JobEnd::Exited { code: 2 },
                },
            ),
            job(
                "j-signaled",
                JobState::Ended {
                    end: JobEnd::Signaled { signal: 15 },
                },
            ),
            job(
                "j-slow",
                JobState::Ended {
                    end: JobEnd::TimedOut { timeout_ms: 5_000 },
                },
            ),
        ];

        let ids: Vec<String> = needs(&report(vec![web]))
            .into_iter()
            .map(|line| match line {
                NeedsYou::Job { id, .. } => id,
                other => panic!("only Jobs fail here: {other:?}"),
            })
            .collect();

        assert_eq!(ids, ["j-red", "j-signaled", "j-slow"]);
    }

    #[test]
    fn a_job_outside_every_repository_needs_you_without_a_worktree() {
        let mut stray = report(Vec::new());
        stray.jobs_outside_repositories = vec![job(
            "j-red",
            JobState::Ended {
                end: JobEnd::Exited { code: 1 },
            },
        )];

        assert_eq!(
            needs(&stray),
            [NeedsYou::Job {
                worktree: None,
                id: "j-red".to_owned(),
                command: "make test".to_owned(),
                end: JobEnd::Exited { code: 1 },
            }]
        );
    }

    #[test]
    fn foreign_processes_are_always_listed_after_the_failures() {
        let mut web = worktree("/p/work/acme/web");
        web.foreign = vec![
            ForeignProcess::Process {
                pid: 4_242,
                command: "node server.js".to_owned(),
                ports: vec![3_000],
            },
            ForeignProcess::Container {
                name: "web-db-1".to_owned(),
                project: "web".to_owned(),
                service: "db".to_owned(),
                ports: vec![5_432],
            },
        ];
        web.services
            .push(ended_service("api", 1_000, ServiceEnd::Exited { code: 1 }));

        let foreign = web.foreign.clone();
        let lines = needs(&report(vec![web]));

        assert!(matches!(lines[0], NeedsYou::Service { .. }));
        assert_eq!(
            lines[1..],
            [
                NeedsYou::Foreign {
                    worktree: PathBuf::from("/p/work/acme/web"),
                    process: foreign[0].clone(),
                },
                NeedsYou::Foreign {
                    worktree: PathBuf::from("/p/work/acme/web"),
                    process: foreign[1].clone(),
                },
            ]
        );
    }

    fn key(started_at: u64) -> FailureKey {
        FailureKey::Service {
            worktree: PathBuf::from("/p/work/acme/web"),
            service: "api".to_owned(),
            started_at,
        }
    }

    fn failed_at(started_at: u64) -> StatusReport {
        let mut web = worktree("/p/work/acme/web");
        web.services
            .push(ended_service("api", started_at, ServiceEnd::Exited { code: 1 }));
        report(vec![web])
    }

    #[test]
    fn a_failure_marked_as_seen_leaves_the_list_and_a_new_start_that_fails_brings_it_back() {
        let mut seen = Seen::default();
        assert!(seen.mark(key(1_000), &failed_at(1_000)));

        assert_eq!(needs_you(&failed_at(1_000), &seen), []);
        // The Service started again at 2 000 and failed again: it is not the one the developer saw.
        assert_eq!(needs_you(&failed_at(2_000), &seen).len(), 1);
    }

    #[test]
    fn a_failure_is_keyed_by_its_worktree_its_service_and_its_start() {
        let mut seen = Seen::default();
        seen.mark(key(1_000), &failed_at(1_000));
        let mut elsewhere = worktree("/p/work/acme/web-fix");
        elsewhere
            .services
            .push(ended_service("api", 1_000, ServiceEnd::Exited { code: 1 }));
        let mut other_service = worktree("/p/work/acme/web");
        other_service
            .services
            .push(ended_service("worker", 1_000, ServiceEnd::Exited { code: 1 }));

        assert_eq!(needs_you(&report(vec![elsewhere]), &seen).len(), 1);
        assert_eq!(needs_you(&report(vec![other_service]), &seen).len(), 1);
    }

    #[test]
    fn a_job_marked_as_seen_stays_seen_by_its_id_wherever_it_is_reported() {
        let mut red = worktree("/p/work/acme/web");
        red.jobs = vec![job(
            "j-red",
            JobState::Ended {
                end: JobEnd::Exited { code: 1 },
            },
        )];
        let red = report(vec![red]);
        let mut seen = Seen::default();

        assert!(seen.mark(FailureKey::Job { id: "j-red".to_owned() }, &red));

        assert_eq!(needs_you(&red, &seen), []);
    }

    #[test]
    fn a_key_the_answer_holds_no_failure_for_cannot_be_marked() {
        let mut seen = Seen::default();

        assert!(!seen.mark(key(1_000), &report(Vec::new())), "no such Service");
        assert!(!seen.mark(key(9_999), &failed_at(1_000)), "another start");
        let mut web = worktree("/p/work/acme/web");
        web.services.push(ended_service("api", 1_000, ServiceEnd::Stopped));
        assert!(!seen.mark(key(1_000), &report(vec![web])), "stopped is not a failure");

        assert_eq!(seen, Seen::default());
    }

    #[test]
    fn a_tick_forgets_the_seen_keys_the_answer_no_longer_holds() {
        let mut seen = Seen::default();
        seen.mark(key(1_000), &failed_at(1_000));
        let mut both = failed_at(1_000);
        both.jobs_outside_repositories = vec![job(
            "j-red",
            JobState::Ended {
                end: JobEnd::Exited { code: 1 },
            },
        )];
        seen.mark(FailureKey::Job { id: "j-red".to_owned() }, &both);

        // The Job is gone from the answer, the Service is still there.
        assert!(seen.prune(&failed_at(1_000)));
        assert_eq!(seen.keys(), [key(1_000)]);
        // Nothing left to forget.
        assert!(!seen.prune(&failed_at(1_000)));
        // The Service started again: its old key is gone, so the old failure cannot hide a later one.
        assert!(seen.prune(&failed_at(2_000)));
        assert_eq!(seen.keys(), []);
    }

    fn update(report: StatusReport) -> Arc<Update> {
        Arc::new(Update {
            at_ms: 1,
            answer: Answer::Response(Response::Status(report)),
        })
    }

    fn state_file() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::Builder::new()
            .prefix("pdk")
            .tempdir()
            .expect("the temporary directory can be created");
        let path = dir.path().join("data").join("seen.json");
        (dir, path)
    }

    #[test]
    fn the_seen_set_survives_a_restart_of_the_app() {
        let (_dir, path) = state_file();
        let first = Attention::open(path.clone());
        let (_, saved) = first.mark(key(1_000), update(failed_at(1_000)));
        saved.expect("the set is written, creating its directory");
        drop(first);

        let second = Attention::open(path);
        let (presented, _) = second.on_answer(update(failed_at(1_000)));

        assert_eq!(presented.needs_you, []);
        assert_eq!(presented.seen, [key(1_000)]);
    }

    #[test]
    fn a_pruned_key_is_gone_from_the_file_too() {
        let (_dir, path) = state_file();
        let attention = Attention::open(path.clone());
        attention
            .mark(key(1_000), update(failed_at(1_000)))
            .1
            .expect("the set is written");

        let (presented, saved) = attention.on_answer(update(report(Vec::new())));
        saved.expect("the pruned set is written");

        assert_eq!(presented.seen, []);
        assert_eq!(Seen::load(&path), Seen::default());
    }

    #[test]
    fn an_answer_that_is_not_a_status_leaves_the_seen_set_alone() {
        let (_dir, path) = state_file();
        let attention = Attention::open(path.clone());
        attention
            .mark(key(1_000), update(failed_at(1_000)))
            .1
            .expect("the set is written");
        let bytes = fs::read(&path).expect("the set was written");

        let gone = Arc::new(Update {
            at_ms: 2,
            answer: Answer::Response(Response::Error {
                message: "no".to_owned(),
            }),
        });
        let (presented, saved) = attention.on_answer(gone);
        saved.expect("nothing to write");

        // A Daemon that stops must not make the developer see every failure again when it comes back.
        assert_eq!(presented.seen, [key(1_000)]);
        assert_eq!(presented.needs_you, []);
        assert_eq!(fs::read(&path).expect("the file is still there"), bytes);
    }

    #[test]
    fn a_file_that_is_missing_or_not_a_seen_set_gives_an_empty_one() {
        let (dir, path) = state_file();
        assert_eq!(Seen::load(&path), Seen::default());

        fs::create_dir_all(path.parent().expect("a file has a directory")).expect("the directory is created");
        fs::write(&path, "{ not json").expect("the file is written");
        assert_eq!(Seen::load(&path), Seen::default());

        fs::write(&path, r#"{"unknown":true}"#).expect("the file is written");
        assert_eq!(Seen::load(&path), Seen::default());
        drop(dir);
    }

    #[test]
    fn the_file_is_replaced_whole_and_no_temporary_copy_is_left() {
        let (_dir, path) = state_file();
        let mut seen = Seen::default();
        seen.mark(key(1_000), &failed_at(1_000));
        seen.save(&path).expect("the set is written");
        seen.mark(key(2_000), &failed_at(2_000));
        seen.prune(&failed_at(2_000));

        seen.save(&path).expect("the set is written again");

        assert_eq!(Seen::load(&path), seen);
        let left: Vec<_> = fs::read_dir(path.parent().expect("a file has a directory"))
            .expect("the directory is readable")
            .map(|entry| entry.expect("an entry is readable").file_name())
            .collect();
        assert_eq!(left, ["seen.json"]);
    }

    #[test]
    fn the_window_receives_the_answer_unchanged_next_to_needs_you_and_the_seen_keys() {
        let (_dir, path) = state_file();
        let attention = Attention::open(path);
        let (presented, _) = attention.mark(key(1_000), update(failed_at(1_000)));

        let json = serde_json::to_value(&presented).expect("the update serializes");

        assert_eq!(json["at_ms"], 1);
        assert_eq!(json["response"]["response"], "status");
        assert_eq!(json["needs_you"], serde_json::json!([]));
        assert_eq!(
            json["seen"],
            serde_json::json!([{
                "kind": "service",
                "worktree": "/p/work/acme/web",
                "service": "api",
                "started_at": 1_000,
            }])
        );

        let (fresh, _) = attention.on_answer(update(failed_at(5_000)));
        let json = serde_json::to_value(&fresh).expect("the update serializes");
        assert_eq!(
            json["needs_you"],
            serde_json::json!([{
                "kind": "service",
                "worktree": "/p/work/acme/web",
                "service": "api",
                "started_at": 5_000,
                "end": { "kind": "exited", "code": 1 },
            }])
        );
    }
}
