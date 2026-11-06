//! What the menu bar menu says, before it is drawn: one line per thing that needs the developer, the ports a Copy
//! Port item can copy, and the last state of the Daemon with its time of day. The native items are built from it
//! (`tray.rs`), so what the menu holds is tested without a menu.

use chrono::{DateTime, TimeZone};
use procly_client::protocol::{ForeignProcess, JobEnd, Response, ServiceEnd, ServiceState, StatusReport};

use crate::glyph::Glyph;
use crate::needs_you::NeedsYou;
use crate::poller::{Answer, Update};

/// The longest a command is shown in a line of the menu.
const COMMAND_WIDTH: usize = 40;

/// What the last answer to a `Status` held that the menu shows. It is kept when the Daemon stops answering, since the
/// menu shows the last state it knows with its time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Snapshot {
    /// When the answer came, in milliseconds since the Unix epoch.
    at_ms: u64,
    /// One line per thing that needs the developer, in the order of "Needs you".
    needs_you: Vec<String>,
    /// The ports of what runs, which Copy Port copies.
    ports: Vec<CopyPort>,
}

/// One entry of the Copy Port submenu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CopyPort {
    pub(crate) label: String,
    pub(crate) port: u16,
}

/// The menu as it is drawn: the header, then what changes the shape of the menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Model {
    /// The first line, which says what the Daemon last said and when. It changes with every answer.
    pub(crate) header: String,
    /// The lines of "Needs you".
    pub(crate) needs_you: Vec<String>,
    /// The entries of Copy Port.
    pub(crate) ports: Vec<CopyPort>,
}

impl Model {
    /// The part of the menu whose change means the native items have to be built again. The header is left out: its
    /// text is changed in place, so that a menu the developer holds open is not replaced under their pointer.
    pub(crate) fn shape(&self) -> (&[String], &[CopyPort]) {
        (&self.needs_you, &self.ports)
    }
}

/// The last name of a path, which names a Worktree in a line.
pub(crate) fn name_of(path: &std::path::Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

pub(crate) fn service_end(end: ServiceEnd) -> String {
    match end {
        ServiceEnd::Exited { code } => format!("exited {code}"),
        ServiceEnd::Signaled { signal } => format!("signaled {signal}"),
        ServiceEnd::HealthTimeout { .. } => "health timeout".to_owned(),
        ServiceEnd::Stopped => "stopped".to_owned(),
        ServiceEnd::Idle { .. } => "idle".to_owned(),
        ServiceEnd::Failed { .. } => "failed".to_owned(),
        ServiceEnd::PortMismatch { .. } => "port mismatch".to_owned(),
        ServiceEnd::Lost => "lost".to_owned(),
        ServiceEnd::Orphaned { .. } => "orphaned".to_owned(),
        ServiceEnd::NotListening { .. } => "not listening".to_owned(),
    }
}

pub(crate) fn job_end(end: JobEnd) -> String {
    match end {
        JobEnd::Exited { code } => format!("exited {code}"),
        JobEnd::Signaled { signal } => format!("signaled {signal}"),
        JobEnd::TimedOut { .. } => "timed out".to_owned(),
    }
}

/// A command cut to [`COMMAND_WIDTH`] characters, so that a line stays one line.
pub(crate) fn short(command: &str) -> String {
    let command = command.split_whitespace().collect::<Vec<_>>().join(" ");
    if command.chars().count() <= COMMAND_WIDTH {
        return command;
    }
    let cut: String = command.chars().take(COMMAND_WIDTH - 1).collect();
    format!("{cut}…")
}

fn ports(ports: &[u16]) -> String {
    ports
        .iter()
        .map(|port| format!(":{port}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The line of one thing that needs the developer.
pub(crate) fn line(item: &NeedsYou) -> String {
    match item {
        NeedsYou::Service {
            worktree, service, end, ..
        } => format!("{}: {service} {}", name_of(worktree), service_end(*end)),
        NeedsYou::Job {
            worktree, command, end, ..
        } => {
            let what = format!("{} {}", short(command), job_end(*end));
            worktree
                .as_ref()
                .map_or(what.clone(), |worktree| format!("{}: {what}", name_of(worktree)))
        }
        NeedsYou::Foreign { worktree, process } => {
            let what = match process {
                ForeignProcess::Process {
                    command, ports: held, ..
                } => {
                    format!("foreign {} {}", short(command), ports(held))
                }
                ForeignProcess::Container { name, ports: held, .. } => {
                    format!("foreign container {name} {}", ports(held))
                }
            };
            format!("{}: {}", name_of(worktree), what.trim_end())
        }
    }
}

/// The ports of what runs, a Service that has started or is restarting and an Isolated container, with the Worktree
/// they belong to.
fn copy_ports(report: &StatusReport) -> Vec<CopyPort> {
    let mut found = Vec::new();
    for worktree in report.repositories.iter().flat_map(|repository| &repository.worktrees) {
        let name = name_of(&worktree.worktree.path);
        for service in &worktree.services {
            if let (Some(port), ServiceState::Starting | ServiceState::Healthy | ServiceState::Restarting) =
                (service.port, service.state)
            {
                found.push(CopyPort {
                    label: format!("{name}: {} {port}", service.service),
                    port,
                });
            }
        }
        for container in &worktree.containers {
            if let Some(port) = container.port {
                found.push(CopyPort {
                    label: format!("{name}: {} {port}", container.service),
                    port,
                });
            }
        }
    }
    found
}

/// What an answer of the poller adds to what the menu knows, nothing when the Daemon did not answer a `Status`.
pub(crate) fn snapshot(update: &Update, needs_you: &[NeedsYou]) -> Option<Snapshot> {
    let Answer::Response(Response::Status(report)) = &update.answer else {
        return None;
    };
    Some(Snapshot {
        at_ms: update.at_ms,
        needs_you: needs_you.iter().map(line).collect(),
        ports: copy_ports(report),
    })
}

/// An absolute time: the time of day when it is the same day as `now_ms`, the date before it otherwise.
pub(crate) fn moment<Zone>(at_ms: u64, now_ms: u64, zone: &Zone) -> String
where
    Zone: TimeZone,
    Zone::Offset: std::fmt::Display,
{
    let in_zone = |ms: u64| {
        i64::try_from(ms)
            .ok()
            .and_then(DateTime::from_timestamp_millis)
            .map(|utc| utc.with_timezone(zone))
    };
    match (in_zone(at_ms), in_zone(now_ms)) {
        (Some(at), Some(now)) if at.date_naive() == now.date_naive() => at.format("%H:%M:%S").to_string(),
        (Some(at), _) => at.format("%Y-%m-%d %H:%M:%S").to_string(),
        (None, _) => String::new(),
    }
}

/// The header of the menu: the state the Daemon last answered with and when, or that no Daemon can be read.
fn header(glyph: Glyph, last: Option<&Snapshot>, when: impl Fn(u64) -> String) -> String {
    match (glyph, last) {
        (Glyph::Outline, None) => "No Daemon".to_owned(),
        (Glyph::Outline, Some(last)) => format!("No Daemon, last state {}", when(last.at_ms)),
        (Glyph::Mark | Glyph::NeedsYou, None) => "Waiting for the Daemon".to_owned(),
        (Glyph::Mark | Glyph::NeedsYou, Some(last)) => match last.needs_you.len() {
            0 => format!("Nothing needs you ({})", when(last.at_ms)),
            count => format!("Needs you: {count} ({})", when(last.at_ms)),
        },
    }
}

/// The menu for the glyph on show and the last state the menu knows. `when` writes an absolute time.
pub(crate) fn model(glyph: Glyph, last: Option<&Snapshot>, when: impl Fn(u64) -> String) -> Model {
    Model {
        header: header(glyph, last, when),
        needs_you: last.map_or_else(Vec::new, |last| last.needs_you.clone()),
        ports: last.map_or_else(Vec::new, |last| last.ports.clone()),
    }
}

/// What choosing an item of the menu does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    /// Open the window.
    Open,
    /// Open the window on Settings.
    Settings,
    /// Quit the app, which stops nothing in the Daemon.
    Quit,
    /// Copy a port to the clipboard.
    CopyPort(u16),
}

/// The ids of the items that are always there.
pub(crate) const OPEN_ID: &str = "tray-open";
pub(crate) const SETTINGS_ID: &str = "tray-settings";
pub(crate) const QUIT_ID: &str = "tray-quit";

/// The id of the line of "Needs you" at `index`, which opens the window.
pub(crate) fn needs_you_id(index: usize) -> String {
    format!("tray-needs-you-{index}")
}

/// The id of the Copy Port entry of `port`.
pub(crate) fn copy_port_id(port: u16) -> String {
    format!("tray-copy-port-{port}")
}

/// The id of the header, which is text and does nothing when chosen.
pub(crate) const HEADER_ID: &str = "tray-header";

/// One item of the native menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Item {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) enabled: bool,
}

impl Item {
    fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            enabled: true,
        }
    }
}

/// One entry of the native menu below its header, in the order they show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Entry {
    Item(Item),
    Separator,
    /// A submenu, which has nothing to open while it has no item.
    Submenu {
        label: &'static str,
        enabled: bool,
        items: Vec<Item>,
    },
}

/// The native menu as it is built: the header, then the entries. `tray.rs` draws it item by item, so what the menu
/// holds is tested here without a menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Layout {
    pub(crate) header: Item,
    pub(crate) entries: Vec<Entry>,
}

/// The items of the menu for `model`: the header, the lines of "Needs you" when there are any, Copy Port, then Open
/// Procly, Settings and Quit, which are always there.
pub(crate) fn layout(model: &Model) -> Layout {
    let mut header = Item::new(HEADER_ID, &model.header);
    header.enabled = false;
    let mut entries = vec![Entry::Separator];
    for (index, line) in model.needs_you.iter().enumerate() {
        entries.push(Entry::Item(Item::new(needs_you_id(index), line)));
    }
    if !model.needs_you.is_empty() {
        entries.push(Entry::Separator);
    }
    entries.push(Entry::Submenu {
        label: "Copy Port",
        enabled: !model.ports.is_empty(),
        items: model
            .ports
            .iter()
            .map(|entry| Item::new(copy_port_id(entry.port), &entry.label))
            .collect(),
    });
    entries.push(Entry::Separator);
    entries.push(Entry::Item(Item::new(OPEN_ID, "Open Procly")));
    entries.push(Entry::Item(Item::new(SETTINGS_ID, "Settings…")));
    entries.push(Entry::Separator);
    entries.push(Entry::Item(Item::new(QUIT_ID, "Quit Procly")));
    Layout { header, entries }
}

/// What the item with `id` does, nothing for an id that is not an item of this menu.
pub(crate) fn action(id: &str) -> Option<Action> {
    match id {
        OPEN_ID => Some(Action::Open),
        SETTINGS_ID => Some(Action::Settings),
        QUIT_ID => Some(Action::Quit),
        _ if id.starts_with("tray-needs-you-") => Some(Action::Open),
        _ => id
            .strip_prefix("tray-copy-port-")
            .and_then(|port| port.parse().ok())
            .map(Action::CopyPort),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use chrono::FixedOffset;
    use procly_client::protocol::{ContainerStatus, RepositoryStatus, ServiceStatus, Worktree, WorktreeStatus};

    use super::*;

    fn at(hour: u32, minute: u32, second: u32) -> u64 {
        let day = chrono::NaiveDate::from_ymd_opt(2026, 11, 5).expect("a date");
        let moment = day.and_hms_opt(hour, minute, second).expect("a time").and_utc();
        u64::try_from(moment.timestamp_millis()).expect("after the epoch")
    }

    fn utc() -> FixedOffset {
        FixedOffset::east_opt(0).expect("an offset")
    }

    fn clock(at_ms: u64) -> String {
        moment(at_ms, at(23, 0, 0), &utc())
    }

    fn service(name: &str, port: Option<u16>, state: ServiceState) -> ServiceStatus {
        ServiceStatus {
            service: name.to_owned(),
            port,
            reattached: false,
            started_at: 1,
            restartable: true,
            state,
        }
    }

    fn worktree(path: &str, services: Vec<ServiceStatus>) -> WorktreeStatus {
        WorktreeStatus {
            worktree: Worktree {
                path: PathBuf::from(path),
                slug: "slug".to_owned(),
                main: false,
            },
            branch: None,
            listed: true,
            services,
            jobs: Vec::new(),
            foreign: Vec::new(),
            containers: Vec::new(),
        }
    }

    fn update(worktrees: Vec<WorktreeStatus>, at_ms: u64) -> Update {
        Update {
            at_ms,
            answer: Answer::Response(Response::Status(StatusReport {
                repositories: vec![RepositoryStatus {
                    git_common_dir: PathBuf::from("/p/work/acme/web/.git"),
                    worktrees,
                }],
                jobs_outside_repositories: Vec::new(),
            })),
        }
    }

    fn failed_service() -> NeedsYou {
        NeedsYou::Service {
            worktree: PathBuf::from("/p/work/acme/web-fix"),
            service: "api".to_owned(),
            started_at: 1,
            end: ServiceEnd::Exited { code: 1 },
        }
    }

    #[test]
    fn a_failed_service_is_one_line_with_its_worktree_and_how_it_ended() {
        assert_eq!(line(&failed_service()), "web-fix: api exited 1");
    }

    #[test]
    fn a_failed_job_is_one_line_with_a_short_command() {
        let job = NeedsYou::Job {
            worktree: Some(PathBuf::from("/p/work/acme/web-fix")),
            id: "j1".to_owned(),
            command: "cargo   test --workspace --locked -- --nocapture --test-threads 1".to_owned(),
            end: JobEnd::TimedOut { timeout_ms: 5_000 },
        };

        let text = line(&job);

        assert!(
            text.starts_with("web-fix: cargo test --workspace --locked -- --no…"),
            "{text}"
        );
        assert!(text.ends_with(" timed out"), "{text}");
        assert_eq!(text.matches('…').count(), 1);
        assert!(!text.contains('\n'));
    }

    #[test]
    fn a_job_outside_any_worktree_has_no_worktree_in_front() {
        let job = NeedsYou::Job {
            worktree: None,
            id: "j1".to_owned(),
            command: "make".to_owned(),
            end: JobEnd::Exited { code: 2 },
        };

        assert_eq!(line(&job), "make exited 2");
    }

    #[test]
    fn a_foreign_process_is_one_line_with_the_ports_it_listens_on() {
        let process = NeedsYou::Foreign {
            worktree: PathBuf::from("/p/work/acme/web-fix"),
            process: ForeignProcess::Process {
                pid: 7,
                command: "node server.js".to_owned(),
                ports: vec![3000, 3001],
            },
        };
        let container = NeedsYou::Foreign {
            worktree: PathBuf::from("/p/work/acme/web-fix"),
            process: ForeignProcess::Container {
                name: "db".to_owned(),
                project: "web".to_owned(),
                service: "db".to_owned(),
                ports: Vec::new(),
            },
        };

        assert_eq!(line(&process), "web-fix: foreign node server.js :3000 :3001");
        assert_eq!(line(&container), "web-fix: foreign container db");
    }

    #[test]
    fn the_snapshot_keeps_the_needs_you_lines_and_the_time_of_the_answer() {
        let answer = update(vec![worktree("/p/work/acme/web-fix", Vec::new())], at(19, 35, 12));

        let snapshot = snapshot(&answer, &[failed_service()]).expect("a Status answer");

        assert_eq!(snapshot.at_ms, at(19, 35, 12));
        assert_eq!(snapshot.needs_you, ["web-fix: api exited 1"]);
    }

    #[test]
    fn an_answer_that_is_not_a_status_adds_nothing() {
        let error = Update {
            at_ms: 1,
            answer: Answer::Response(Response::Error {
                message: "no".to_owned(),
            }),
        };

        assert_eq!(snapshot(&error, &[]), None);
    }

    #[test]
    fn copy_port_lists_the_ports_of_what_runs_and_none_of_what_ended() {
        let mut web = worktree(
            "/p/work/acme/web-fix",
            vec![
                service("api", Some(24001), ServiceState::Healthy),
                service("web", Some(24002), ServiceState::Restarting),
                service(
                    "old",
                    Some(24003),
                    ServiceState::Ended {
                        end: ServiceEnd::Stopped,
                    },
                ),
                service("worker", None, ServiceState::Healthy),
            ],
        );
        web.containers.push(ContainerStatus {
            service: "db".to_owned(),
            project: "p".to_owned(),
            compose: "postgres".to_owned(),
            port: Some(24010),
        });
        let answer = update(vec![web], at(19, 0, 0));

        let ports = snapshot(&answer, &[]).expect("a Status answer").ports;

        assert_eq!(
            ports,
            [
                CopyPort {
                    label: "web-fix: api 24001".to_owned(),
                    port: 24001
                },
                CopyPort {
                    label: "web-fix: web 24002".to_owned(),
                    port: 24002
                },
                CopyPort {
                    label: "web-fix: db 24010".to_owned(),
                    port: 24010
                },
            ]
        );
    }

    #[test]
    fn a_time_of_the_same_day_shows_the_time_of_day_and_an_earlier_day_adds_the_date() {
        assert_eq!(moment(at(19, 35, 12), at(23, 0, 0), &utc()), "19:35:12");
        assert_eq!(
            moment(at(19, 35, 12), at(23, 0, 0) + 86_400_000, &utc()),
            "2026-11-05 19:35:12"
        );
    }

    #[test]
    fn the_time_is_the_time_of_day_in_the_zone_given() {
        let paris = FixedOffset::east_opt(3600).expect("an offset");

        assert_eq!(moment(at(19, 35, 12), at(21, 0, 0), &paris), "20:35:12");
        // 23:30 UTC is already the next day in Paris.
        assert_eq!(moment(at(19, 35, 12), at(23, 30, 0), &paris), "2026-11-05 20:35:12");
    }

    fn known(needs_you: &[NeedsYou]) -> Snapshot {
        let answer = update(
            vec![worktree(
                "/p/work/acme/web-fix",
                vec![service("api", Some(24001), ServiceState::Healthy)],
            )],
            at(19, 35, 12),
        );
        snapshot(&answer, needs_you).expect("a Status answer")
    }

    #[test]
    fn the_header_says_nothing_needs_the_developer_with_the_time_of_the_last_state() {
        let model = model(Glyph::Mark, Some(&known(&[])), clock);

        assert_eq!(model.header, "Nothing needs you (19:35:12)");
        assert_eq!(model.needs_you, Vec::<String>::new());
    }

    #[test]
    fn the_header_counts_what_needs_the_developer_and_the_menu_lists_it() {
        let model = model(Glyph::NeedsYou, Some(&known(&[failed_service()])), clock);

        assert_eq!(model.header, "Needs you: 1 (19:35:12)");
        assert_eq!(model.needs_you, ["web-fix: api exited 1"]);
        assert_eq!(model.ports.len(), 1);
    }

    #[test]
    fn without_a_daemon_the_header_gives_the_last_state_with_its_time_and_the_menu_keeps_it() {
        let last = known(&[failed_service()]);

        let model = model(Glyph::Outline, Some(&last), clock);

        assert_eq!(model.header, "No Daemon, last state 19:35:12");
        assert_eq!(model.needs_you, ["web-fix: api exited 1"]);
    }

    #[test]
    fn before_any_answer_the_header_waits_and_without_a_daemon_it_says_so() {
        assert_eq!(model(Glyph::Mark, None, clock).header, "Waiting for the Daemon");
        assert_eq!(model(Glyph::Outline, None, clock).header, "No Daemon");
    }

    #[test]
    fn a_new_time_alone_does_not_change_the_shape_of_the_menu() {
        let first = model(Glyph::Mark, Some(&known(&[])), clock);
        let mut later = known(&[]);
        later.at_ms = at(19, 40, 0);
        let second = model(Glyph::Mark, Some(&later), clock);

        assert_ne!(first.header, second.header);
        assert_eq!(first.shape(), second.shape());
    }

    #[test]
    fn the_items_of_the_menu_map_to_their_action() {
        assert_eq!(action(OPEN_ID), Some(Action::Open));
        assert_eq!(action(SETTINGS_ID), Some(Action::Settings));
        assert_eq!(action(QUIT_ID), Some(Action::Quit));
        assert_eq!(action(&needs_you_id(3)), Some(Action::Open));
        assert_eq!(action(&copy_port_id(24001)), Some(Action::CopyPort(24001)));
    }

    #[test]
    fn an_id_that_is_not_in_this_menu_does_nothing() {
        assert_eq!(action("refresh"), None);
        assert_eq!(action("settings"), None);
        assert_eq!(action("tray-copy-port-lots"), None);
        assert_eq!(action("tray-copy-port-99999"), None);
    }

    fn item(id: &str, label: &str) -> Entry {
        Entry::Item(Item::new(id, label))
    }

    #[test]
    fn the_menu_holds_the_header_what_needs_you_copy_port_open_settings_and_quit_in_that_order() {
        let model = model(Glyph::NeedsYou, Some(&known(&[failed_service()])), clock);

        let built = layout(&model);

        assert_eq!(
            built.header,
            Item {
                id: HEADER_ID.to_owned(),
                label: "Needs you: 1 (19:35:12)".to_owned(),
                enabled: false,
            }
        );
        assert_eq!(
            built.entries,
            [
                Entry::Separator,
                item("tray-needs-you-0", "web-fix: api exited 1"),
                Entry::Separator,
                Entry::Submenu {
                    label: "Copy Port",
                    enabled: true,
                    items: vec![Item::new("tray-copy-port-24001", &model.ports[0].label)],
                },
                Entry::Separator,
                item(OPEN_ID, "Open Procly"),
                item(SETTINGS_ID, "Settings…"),
                Entry::Separator,
                item(QUIT_ID, "Quit Procly"),
            ]
        );
    }

    #[test]
    fn with_nothing_to_show_the_menu_keeps_open_settings_and_quit_and_disables_copy_port() {
        let built = layout(&model(Glyph::Outline, None, clock));

        assert_eq!(built.header.label, "No Daemon");
        assert_eq!(
            built.entries,
            [
                Entry::Separator,
                Entry::Submenu {
                    label: "Copy Port",
                    enabled: false,
                    items: Vec::new(),
                },
                Entry::Separator,
                item(OPEN_ID, "Open Procly"),
                item(SETTINGS_ID, "Settings…"),
                Entry::Separator,
                item(QUIT_ID, "Quit Procly"),
            ]
        );
    }

    #[test]
    fn every_item_the_menu_builds_does_something_except_the_header() {
        let built = layout(&model(Glyph::NeedsYou, Some(&known(&[failed_service()])), clock));
        let clickable = built.entries.iter().flat_map(|entry| match entry {
            Entry::Item(item) => vec![item],
            Entry::Submenu { items, .. } => items.iter().collect(),
            Entry::Separator => Vec::new(),
        });

        for item in clickable {
            assert!(action(&item.id).is_some(), "{} does nothing", item.id);
        }
        assert_eq!(action(&built.header.id), None);
    }
}
