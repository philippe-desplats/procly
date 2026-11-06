//! Failure notifications: a banner when a failure enters "Needs you" and the developer asked for it.
//!
//! Which failures are new is decided here, from the answers of the poller, and the banners are sent from Rust only:
//! the window holds no `notification` permission. Nothing asks for the permission at launch, it is asked when the
//! developer turns "Notify on failure" on. The desktop plugin answers granted whatever the system says, so macOS
//! decides whether a banner shows (`docs/rules/tauri.md`).

use std::collections::{BTreeSet, HashMap};
use std::io;

use procly_client::protocol::{Response, StatusReport};
use serde::{Serialize, Serializer};
use tauri::{AppHandle, Emitter as _, State};
use tauri_plugin_notification::{NotificationExt as _, PermissionState};

use crate::needs_you::{FailureKey, NeedsYou, Place, Presented, report_of};
use crate::poller::Answer;
use crate::settings::{SettingsError, SettingsStore};
use crate::tray_menu::{job_end, label_of, service_end, short};
use crate::window::MainWindow;

/// The most failures of one answer that get a banner each. More than that give one banner for all of them.
const SEPARATE_BANNERS: usize = 3;

/// The event that tells the window a banner or a line of "Needs you" in the menu was clicked. Its payload is the
/// [`Place`] of the item it was about, or nothing for a banner about several failures.
pub(crate) const OPEN_FAILURE: &str = "open-failure";

/// The field of a banner that carries the [`Place`] of its failure back when the banner is clicked.
const PLACE_FIELD: &str = "place";

/// What the plugin reports for a click on the banner itself.
const TAP: &str = "tap";

/// The pane of System Settings where the developer allows banners for the app.
const SYSTEM_NOTIFICATION_SETTINGS: &str = "x-apple.systempreferences:com.apple.Notifications-Settings.extension";

/// What the developer is told when a failure enters "Needs you".
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Banner {
    pub(crate) title: String,
    pub(crate) body: String,
    /// The failure a click opens the window on, nothing for a banner about several of them.
    pub(crate) place: Option<Place>,
}

/// Which failures an answer brings that the previous answer did not hold.
///
/// The first answer after launch is a baseline, and so is the first one after the Daemon did not answer, since a
/// Daemon that restarted starts every Service again and all its failures would look new. The set is replaced by each
/// answer, so a failure that leaves "Needs you", marked as Seen or gone, comes back as new if it enters again.
#[derive(Debug, Default)]
struct Tracker {
    known: Option<BTreeSet<FailureKey>>,
}

impl Tracker {
    /// The keys of `current` that entered "Needs you" with this answer, in the order of `current`.
    fn entering(&mut self, answer: &Answer, current: &[FailureKey]) -> Vec<FailureKey> {
        match answer {
            // The Daemon did not answer, so what it answers next is read as a first answer again.
            Answer::Failure(_) => {
                self.known = None;
                Vec::new()
            }
            Answer::Response(Response::Status(_)) => {
                let entered = self.known.as_ref().map_or_else(Vec::new, |known| {
                    current.iter().filter(|key| !known.contains(key)).cloned().collect()
                });
                self.known = Some(current.iter().cloned().collect());
                entered
            }
            // Only a `Status` says what fails, any other answer leaves the set as it is.
            Answer::Response(_) => Vec::new(),
        }
    }
}

/// Follows the answers of the poller and works out the banners they call for.
#[derive(Debug, Default)]
pub(crate) struct Notifier {
    tracker: Tracker,
}

impl Notifier {
    /// The banners an answer calls for. The answers are followed whether `enabled` or not, so that a developer who
    /// turns the setting on is told of the failures that come after, not of the ones already there.
    pub(crate) fn banners(&mut self, presented: &Presented, enabled: bool) -> Vec<Banner> {
        let current = presented.unseen_failures();
        let keys: Vec<FailureKey> = current.iter().map(|(key, _)| key.clone()).collect();
        let entered = self.tracker.entering(&presented.update().answer, &keys);
        let Some(report) = report_of(presented.update()) else {
            return Vec::new();
        };
        if !enabled {
            return Vec::new();
        }
        let failed: Vec<&NeedsYou> = current
            .iter()
            .filter(|(key, _)| entered.contains(key))
            .map(|(_, item)| item)
            .collect();
        banners_of(&failed, report)
    }
}

/// One banner per failure up to [`SEPARATE_BANNERS`], one for all of them beyond.
fn banners_of(failed: &[&NeedsYou], report: &StatusReport) -> Vec<Banner> {
    let each: Vec<Banner> = failed.iter().filter_map(|item| banner_of(item, report)).collect();
    if each.len() > SEPARATE_BANNERS {
        return vec![Banner {
            title: format!("{} failures need you", each.len()),
            body: "Open Procly to see them.".to_owned(),
            place: None,
        }];
    }
    each
}

/// The banner of one failure. A Foreign process is not a failure and has none.
fn banner_of(item: &NeedsYou, report: &StatusReport) -> Option<Banner> {
    match item {
        NeedsYou::Service {
            worktree, service, end, ..
        } => Some(Banner {
            title: format!("{service}: {}", service_end(*end)),
            body: label_of(report, worktree),
            place: Some(item.place()),
        }),
        NeedsYou::Job {
            worktree, command, end, ..
        } => Some(Banner {
            title: format!("{}: {}", short(command), job_end(*end)),
            body: worktree
                .as_deref()
                .map_or_else(|| "Outside any Worktree".to_owned(), |path| label_of(report, path)),
            place: Some(item.place()),
        }),
        NeedsYou::Foreign { .. } => None,
    }
}

/// The fields a banner carries back when it is clicked.
fn extra_of(banner: &Banner) -> Option<(&'static str, serde_json::Value)> {
    let place = banner.place.as_ref()?;
    serde_json::to_value(place).ok().map(|value| (PLACE_FIELD, value))
}

/// The failure a clicked banner was about, from the fields it carried back. Nothing for a grouped banner, or one this
/// app did not write.
fn place_of(extra: &HashMap<String, serde_json::Value>) -> Option<Place> {
    extra
        .get(PLACE_FIELD)
        .and_then(|value| serde_json::from_value(value.clone()).ok())
}

/// Shows `banner`. A click on it brings the window back, on its failure.
pub(crate) fn show(app: &AppHandle, banner: &Banner) {
    let mut builder = app.notification().builder().title(&banner.title).body(&banner.body);
    if let Some((field, value)) = extra_of(banner) {
        builder = builder.extra(field, value);
    }
    if let Err(error) = builder.show() {
        eprintln!("procly: cannot show a notification: {error}");
    }
}

/// Opens the window on the failure of a clicked banner. Registered once, before any banner shows, since the plugin
/// reports a click only for banners shown after a handler exists.
pub(crate) fn open_on_click(app: &AppHandle) {
    let handle = app.clone();
    let registered = app.notification().on_action(move |performed| {
        if performed.action_id() != TAP {
            return;
        }
        let place = performed.notification().and_then(|banner| place_of(banner.extra()));
        MainWindow(&handle).open();
        // The window listens while it is hidden too, and shows the failure once it is back.
        let _ = handle.emit(OPEN_FAILURE, place);
    });
    if let Err(error) = registered {
        eprintln!("procly: banners cannot be clicked: {error}");
    }
}

/// The permission to show banners, as the system answers a request for it.
pub(crate) trait Permission {
    /// Asks for the permission, and tells whether it was granted.
    fn request(&self) -> Result<bool, NotifyError>;
}

impl Permission for AppHandle {
    fn request(&self) -> Result<bool, NotifyError> {
        self.notification()
            .request_permission()
            .map(|state| state == PermissionState::Granted)
            .map_err(|error| NotifyError::Permission(error.to_string()))
    }
}

/// Turns "Notify on failure" on or off and answers the state it ends up in.
///
/// The permission is asked when it is turned on, never when it is turned off or at launch. A refusal leaves it off.
///
/// # Errors
///
/// When the permission cannot be asked, or the setting cannot be written.
pub(crate) fn switch(permission: &impl Permission, store: &SettingsStore, enabled: bool) -> Result<bool, NotifyError> {
    let on = enabled && permission.request()?;
    store.set_notify_on_failure(on)?;
    Ok(on)
}

/// The system, as far as showing its notification settings goes.
pub(crate) trait SystemSettings {
    /// Opens the pane of System Settings that lists the banners of the apps.
    fn open_notifications(&self) -> io::Result<()>;
}

/// The `opener` plugin, which asks the system to open the pane.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SystemOpener;

impl SystemSettings for SystemOpener {
    fn open_notifications(&self) -> io::Result<()> {
        tauri_plugin_opener::open_url(SYSTEM_NOTIFICATION_SETTINGS, None::<&str>).map_err(io::Error::other)
    }
}

/// The permission could not be asked, the setting could not be written, or the pane could not be opened.
#[derive(Debug, thiserror::Error)]
pub(crate) enum NotifyError {
    #[error("cannot ask for the permission to notify: {0}")]
    Permission(String),
    #[error(transparent)]
    Settings(#[from] SettingsError),
    #[error("cannot open the notification settings of macOS: {0}")]
    Open(#[source] io::Error),
    #[error("cannot change the notifications: {0}")]
    Task(String),
}

impl NotifyError {
    /// The thread that talks to the system stopped before it could.
    pub(crate) fn task(error: &tauri::Error) -> Self {
        Self::Task(error.to_string())
    }
}

impl Serialize for NotifyError {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Failure {
            kind: &'static str,
            message: String,
        }
        match self {
            Self::Settings(error) => error.serialize(serializer),
            Self::Permission(_) | Self::Open(_) | Self::Task(_) => Failure {
                kind: "notifications",
                message: self.to_string(),
            }
            .serialize(serializer),
        }
    }
}

/// Turns "Notify on failure" on or off, and answers whether it is on. Turning it on asks for the permission, and a
/// refusal answers off.
///
/// # Errors
///
/// When the permission cannot be asked or the setting cannot be written.
#[tauri::command]
pub(crate) async fn set_notify_on_failure(
    enabled: bool,
    app: AppHandle,
    store: State<'_, SettingsStore>,
) -> Result<bool, NotifyError> {
    let store = store.inner().clone();
    // The system is asked from a thread that may block, and the file is written there too.
    tauri::async_runtime::spawn_blocking(move || switch(&app, &store, enabled))
        .await
        .map_err(|error| NotifyError::task(&error))?
}

/// Opens the notification settings of macOS, where the developer allows the banners of the app.
///
/// # Errors
///
/// When the system does not open the pane.
#[tauri::command]
pub(crate) async fn open_notification_settings() -> Result<(), NotifyError> {
    tauri::async_runtime::spawn_blocking(|| SystemOpener.open_notifications().map_err(NotifyError::Open))
        .await
        .map_err(|error| NotifyError::task(&error))?
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::path::PathBuf;
    use std::sync::Arc;

    use procly_client::protocol::{
        ForeignProcess, JobEnd, JobState, JobStatus, RepositoryStatus, ServiceEnd, ServiceState, ServiceStatus,
        Worktree, WorktreeStatus,
    };

    use super::*;
    use crate::needs_you::Attention;
    use crate::poller::Update;

    fn worktree(path: &str, branch: Option<&str>) -> WorktreeStatus {
        WorktreeStatus {
            worktree: Worktree {
                path: PathBuf::from(path),
                slug: "web".to_owned(),
                main: false,
            },
            branch: branch.map(str::to_owned),
            listed: true,
            services: Vec::new(),
            jobs: Vec::new(),
            foreign: Vec::new(),
            containers: Vec::new(),
        }
    }

    fn service(name: &str, started_at: u64, state: ServiceState) -> ServiceStatus {
        ServiceStatus {
            service: name.to_owned(),
            port: None,
            reattached: false,
            started_at,
            restartable: false,
            state,
        }
    }

    fn crashed(name: &str, started_at: u64) -> ServiceStatus {
        service(
            name,
            started_at,
            ServiceState::Ended {
                end: ServiceEnd::Exited { code: 1 },
            },
        )
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

    fn with_services(services: Vec<ServiceStatus>) -> StatusReport {
        let mut web = worktree("/p/work/acme/web", Some("fix-login"));
        web.services = services;
        report(vec![web])
    }

    /// A poller, a Seen set and a notifier, as the app has them.
    struct Rig {
        _dir: tempfile::TempDir,
        attention: Attention,
        notifier: Notifier,
    }

    impl Rig {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("a temporary directory");
            let attention = Attention::open(dir.path().join("seen.json"));
            Self {
                _dir: dir,
                attention,
                notifier: Notifier::default(),
            }
        }

        fn presented(&self, answer: Answer) -> Presented {
            self.attention.present_update(Arc::new(Update { at_ms: 1, answer }))
        }

        fn answer(&mut self, report: StatusReport, enabled: bool) -> Vec<Banner> {
            let presented = self.presented(Answer::Response(Response::Status(report)));
            self.notifier.banners(&presented, enabled)
        }

        fn daemon_gone(&mut self) {
            let presented = self.presented(Answer::Failure(crate::daemon::CommandError::Client(
                procly_client::ClientError::Unreachable {
                    path: PathBuf::from("/p/state/procly.sock"),
                    source: io::Error::from(io::ErrorKind::NotFound),
                },
            )));
            assert_eq!(self.notifier.banners(&presented, true), []);
        }
    }

    fn titles(banners: &[Banner]) -> Vec<&str> {
        banners.iter().map(|banner| banner.title.as_str()).collect()
    }

    #[test]
    fn a_failure_that_enters_needs_you_makes_a_banner_with_its_worktree_and_how_it_ended() {
        let mut rig = Rig::new();
        rig.answer(with_services(vec![]), true);

        let banners = rig.answer(with_services(vec![crashed("api", 1_000)]), true);

        assert_eq!(
            banners,
            [Banner {
                title: "api: exited 1".to_owned(),
                body: "fix-login".to_owned(),
                place: Some(Place::Service {
                    worktree: PathBuf::from("/p/work/acme/web"),
                    service: "api".to_owned(),
                }),
            }]
        );
    }

    #[test]
    fn a_clicked_banner_gives_back_the_failure_it_was_about_and_a_grouped_one_gives_nothing() {
        let place = Place::Job {
            worktree: None,
            id: "job-2".to_owned(),
        };
        let banner = Banner {
            title: "make test: exited 2".to_owned(),
            body: "Outside any Worktree".to_owned(),
            place: Some(place.clone()),
        };
        let grouped = Banner {
            place: None,
            ..banner.clone()
        };

        let carried: HashMap<String, serde_json::Value> = extra_of(&banner)
            .map(|(field, value)| (field.to_owned(), value))
            .into_iter()
            .collect();

        assert_eq!(place_of(&carried), Some(place));
        assert_eq!(extra_of(&grouped), None);
        assert_eq!(place_of(&HashMap::new()), None);
    }

    #[test]
    fn a_failure_is_announced_once_however_long_it_stays() {
        let mut rig = Rig::new();
        rig.answer(with_services(vec![]), true);
        assert_eq!(rig.answer(with_services(vec![crashed("api", 1_000)]), true).len(), 1);

        assert_eq!(rig.answer(with_services(vec![crashed("api", 1_000)]), true), []);
        assert_eq!(rig.answer(with_services(vec![crashed("api", 1_000)]), true), []);
    }

    #[test]
    fn the_first_answer_after_launch_is_a_baseline_and_announces_nothing() {
        let mut rig = Rig::new();

        let banners = rig.answer(with_services(vec![crashed("api", 1_000), crashed("web", 1_000)]), true);

        assert_eq!(banners, []);
    }

    #[test]
    fn the_first_answer_after_the_daemon_answers_again_is_a_baseline() {
        let mut rig = Rig::new();
        rig.answer(with_services(vec![]), true);
        rig.daemon_gone();

        // The Daemon restarted and its Services started again, one of them already failed.
        let banners = rig.answer(with_services(vec![crashed("api", 2_000)]), true);
        assert_eq!(banners, []);

        // The answer after the baseline notifies as usual.
        let banners = rig.answer(with_services(vec![crashed("api", 2_000), crashed("web", 2_500)]), true);
        assert_eq!(titles(&banners), ["web: exited 1"]);
    }

    #[test]
    fn a_service_that_fails_again_after_a_new_start_is_a_new_failure() {
        let mut rig = Rig::new();
        rig.answer(with_services(vec![crashed("api", 1_000)]), true);

        let running = service("api", 2_000, ServiceState::Healthy);
        assert_eq!(rig.answer(with_services(vec![running]), true), []);
        let banners = rig.answer(with_services(vec![crashed("api", 3_000)]), true);

        assert_eq!(titles(&banners), ["api: exited 1"]);
    }

    #[test]
    fn a_routine_end_is_not_a_failure() {
        let mut rig = Rig::new();
        rig.answer(with_services(vec![]), true);

        let stopped = service(
            "api",
            1_000,
            ServiceState::Ended {
                end: ServiceEnd::Stopped,
            },
        );
        let done = service(
            "web",
            1_000,
            ServiceState::Ended {
                end: ServiceEnd::Exited { code: 0 },
            },
        );

        assert_eq!(rig.answer(with_services(vec![stopped, done]), true), []);
    }

    #[test]
    fn a_foreign_process_never_makes_a_banner() {
        let mut rig = Rig::new();
        rig.answer(with_services(vec![]), true);
        let mut web = worktree("/p/work/acme/web", None);
        web.foreign = vec![
            ForeignProcess::Process {
                pid: 4_321,
                command: "node server.js".to_owned(),
                ports: vec![3_000],
            },
            ForeignProcess::Container {
                name: "db-1".to_owned(),
                project: "web".to_owned(),
                service: "db".to_owned(),
                ports: vec![5_432],
            },
        ];

        assert_eq!(rig.answer(report(vec![web]), true), []);
    }

    #[test]
    fn a_foreign_process_has_no_banner_even_when_asked_for_one() {
        let foreign = NeedsYou::Foreign {
            worktree: PathBuf::from("/p/work/acme/web"),
            process: ForeignProcess::Process {
                pid: 4_321,
                command: "node server.js".to_owned(),
                ports: vec![3_000],
            },
        };

        assert_eq!(banner_of(&foreign, &with_services(vec![])), None);
    }

    #[test]
    fn up_to_3_failures_in_one_answer_make_a_banner_each() {
        let mut rig = Rig::new();
        rig.answer(with_services(vec![]), true);

        let banners = rig.answer(
            with_services(vec![crashed("api", 1_000), crashed("web", 1_000), crashed("db", 1_000)]),
            true,
        );

        assert_eq!(titles(&banners), ["api: exited 1", "web: exited 1", "db: exited 1"]);
    }

    #[test]
    fn more_than_3_failures_in_one_answer_make_one_grouped_banner() {
        let mut rig = Rig::new();
        rig.answer(with_services(vec![]), true);

        let banners = rig.answer(
            with_services(vec![
                crashed("api", 1_000),
                crashed("web", 1_000),
                crashed("db", 1_000),
                crashed("queue", 1_000),
            ]),
            true,
        );

        assert_eq!(
            banners,
            [Banner {
                title: "4 failures need you".to_owned(),
                body: "Open Procly to see them.".to_owned(),
                place: None,
            }]
        );
    }

    #[test]
    fn failures_known_before_are_not_counted_in_the_group() {
        let mut rig = Rig::new();
        rig.answer(with_services(vec![crashed("api", 1_000), crashed("web", 1_000)]), true);

        let banners = rig.answer(
            with_services(vec![
                crashed("api", 1_000),
                crashed("web", 1_000),
                crashed("db", 1_000),
                crashed("queue", 1_000),
            ]),
            true,
        );

        assert_eq!(titles(&banners), ["db: exited 1", "queue: exited 1"]);
    }

    #[test]
    fn a_failed_job_makes_a_banner_with_its_command_and_an_outside_job_opens_with_no_worktree() {
        let mut rig = Rig::new();
        rig.answer(with_services(vec![]), true);
        let job = |id: &str| JobStatus {
            id: id.to_owned(),
            command: "make test".to_owned(),
            cwd: PathBuf::from("/p/work/acme/web"),
            state: JobState::Ended {
                end: JobEnd::Exited { code: 2 },
            },
        };
        let mut web = worktree("/p/work/acme/web", None);
        web.jobs = vec![job("job-1")];
        let mut answer = report(vec![web]);
        answer.jobs_outside_repositories = vec![job("job-2")];

        let banners = rig.answer(answer, true);

        assert_eq!(
            banners,
            [
                Banner {
                    title: "make test: exited 2".to_owned(),
                    body: "web".to_owned(),
                    place: Some(Place::Job {
                        worktree: Some(PathBuf::from("/p/work/acme/web")),
                        id: "job-1".to_owned(),
                    }),
                },
                Banner {
                    title: "make test: exited 2".to_owned(),
                    body: "Outside any Worktree".to_owned(),
                    place: Some(Place::Job {
                        worktree: None,
                        id: "job-2".to_owned(),
                    }),
                },
            ]
        );
    }

    #[test]
    fn a_worktree_without_a_branch_is_named_by_the_last_name_of_its_path() {
        let mut rig = Rig::new();
        rig.answer(with_services(vec![]), true);
        let mut web = worktree("/p/work/acme/web-fix", None);
        web.services = vec![crashed("api", 1_000)];

        let banners = rig.answer(report(vec![web]), true);

        assert_eq!(banners[0].body, "web-fix");
    }

    #[test]
    fn nothing_is_shown_while_the_setting_is_off() {
        let mut rig = Rig::new();
        rig.answer(with_services(vec![]), false);

        assert_eq!(rig.answer(with_services(vec![crashed("api", 1_000)]), false), []);
    }

    #[test]
    fn turning_the_setting_on_announces_the_failures_that_come_after_and_not_the_ones_already_there() {
        let mut rig = Rig::new();
        rig.answer(with_services(vec![]), false);
        rig.answer(with_services(vec![crashed("api", 1_000)]), false);

        let banners = rig.answer(with_services(vec![crashed("api", 1_000), crashed("web", 2_000)]), true);

        assert_eq!(titles(&banners), ["web: exited 1"]);
    }

    #[test]
    fn a_failure_the_developer_marked_as_seen_does_not_come_back_as_a_new_one() {
        let mut rig = Rig::new();
        let both = || with_services(vec![crashed("api", 1_000), crashed("web", 1_000)]);
        rig.answer(with_services(vec![]), true);
        let update = Arc::new(Update {
            at_ms: 1,
            answer: Answer::Response(Response::Status(both())),
        });
        let presented = rig.attention.present_update(Arc::clone(&update));
        assert_eq!(rig.notifier.banners(&presented, true).len(), 2);

        // The developer saw `api`, so it leaves "Needs you" while `web` stays.
        let key = FailureKey::Service {
            worktree: PathBuf::from("/p/work/acme/web"),
            service: "api".to_owned(),
            started_at: 1_000,
        };
        let (after_mark, _) = rig.attention.mark(key, update);
        assert_eq!(rig.notifier.banners(&after_mark, true), []);
        assert_eq!(rig.answer(both(), true), []);
    }

    #[test]
    fn an_answer_that_is_not_a_status_leaves_the_followed_failures_alone() {
        let mut rig = Rig::new();
        rig.answer(with_services(vec![crashed("api", 1_000)]), true);

        let other = rig.presented(Answer::Response(Response::Down {
            stopped: Vec::new(),
            released: Vec::new(),
        }));
        assert_eq!(rig.notifier.banners(&other, true), []);

        assert_eq!(rig.answer(with_services(vec![crashed("api", 1_000)]), true), []);
    }

    /// The system, which answers a request for the permission as it is told to and counts the requests.
    struct System {
        grants: bool,
        asked: Cell<usize>,
        fails: bool,
    }

    impl System {
        fn granting() -> Self {
            Self {
                grants: true,
                asked: Cell::new(0),
                fails: false,
            }
        }
    }

    impl Permission for System {
        fn request(&self) -> Result<bool, NotifyError> {
            self.asked.set(self.asked.get() + 1);
            if self.fails {
                return Err(NotifyError::Permission("the system did not answer".to_owned()));
            }
            Ok(self.grants)
        }
    }

    fn store() -> (tempfile::TempDir, SettingsStore) {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let store = SettingsStore::open(dir.path().join("settings.json"));
        (dir, store)
    }

    #[test]
    fn the_permission_is_asked_when_the_switch_is_turned_on_and_not_before() {
        let (_dir, store) = store();
        let system = System::granting();

        // Opening the settings and leaving them off asks nothing.
        assert!(!store.get().notify_on_failure);
        assert_eq!(system.asked.get(), 0);

        assert!(switch(&system, &store, true).expect("the switch"));
        assert_eq!(system.asked.get(), 1);
        assert!(store.get().notify_on_failure);
    }

    #[test]
    fn a_refused_permission_leaves_the_switch_off() {
        let (_dir, store) = store();
        let system = System {
            grants: false,
            ..System::granting()
        };

        assert!(!switch(&system, &store, true).expect("the switch"));

        assert!(!store.get().notify_on_failure);
    }

    #[test]
    fn turning_the_switch_off_asks_for_nothing() {
        let (_dir, store) = store();
        let system = System::granting();
        switch(&system, &store, true).expect("the switch on");

        assert!(!switch(&system, &store, false).expect("the switch off"));

        assert_eq!(system.asked.get(), 1);
        assert!(!store.get().notify_on_failure);
    }

    #[test]
    fn a_permission_that_cannot_be_asked_is_an_error_and_leaves_the_switch_as_it_was() {
        let (_dir, store) = store();
        let system = System {
            fails: true,
            ..System::granting()
        };

        let error = switch(&system, &store, true).expect_err("the system did not answer");

        assert!(error.to_string().contains("did not answer"), "{error}");
        assert!(!store.get().notify_on_failure);
    }

    #[derive(Default)]
    struct Pane {
        opened: RefCell<Vec<&'static str>>,
    }

    impl SystemSettings for Pane {
        fn open_notifications(&self) -> io::Result<()> {
            self.opened.borrow_mut().push(SYSTEM_NOTIFICATION_SETTINGS);
            Ok(())
        }
    }

    #[test]
    fn the_notification_settings_of_the_system_open_on_the_pane_of_the_notifications() {
        let pane = Pane::default();

        pane.open_notifications().expect("the pane opens");

        assert_eq!(
            *pane.opened.borrow(),
            ["x-apple.systempreferences:com.apple.Notifications-Settings.extension"]
        );
    }

    #[test]
    fn an_error_reaches_the_window_with_a_kind_and_a_message() {
        let json = serde_json::to_value(NotifyError::Permission("no answer".to_owned())).expect("serializes");

        assert_eq!(json["kind"], "notifications");
        assert!(
            json["message"]
                .as_str()
                .is_some_and(|message| message.contains("no answer"))
        );
    }
}
