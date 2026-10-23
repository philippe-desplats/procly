//! The `--json` output of every command: one document on stdout, rendered client side from the same response as the
//! text, so that the Daemon knows nothing of it.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Context as _;
use serde::{Serialize, Serializer};

use crate::doctor::Report;
use crate::protocol::{
    Check, Conflict, InitReport, JobEnd, KeptLease, KeptResource, LeaseHeld, Left, ReleaseReport, Released, Removed,
    ServiceEnd, StartOutcome, Stopped, UpService,
};

/// The version of the `--json` schema of every command, bumped on any incompatible change, independently of the
/// socket protocol.
const SCHEMA_VERSION: u32 = 1;

/// Renders `document` as one line of JSON that starts with the `schema_version`, newline included.
pub(crate) fn render<T: Serialize>(document: &T) -> Result<String, serde_json::Error> {
    #[derive(Serialize)]
    struct Versioned<'a, T> {
        schema_version: u32,
        #[serde(flatten)]
        document: &'a T,
    }
    let mut json = serde_json::to_string(&Versioned {
        schema_version: SCHEMA_VERSION,
        document,
    })?;
    json.push('\n');
    Ok(json)
}

/// Prints `document` on stdout.
pub(crate) fn print<T: Serialize>(document: &T) -> anyhow::Result<()> {
    crate::print(&render(document).context("cannot render the JSON output")?)
}

/// What `run` prints: the Job and whether it was started or found running.
#[derive(Debug, Serialize)]
pub(crate) struct Run<'a> {
    pub(crate) job: &'a str,
    pub(crate) outcome: StartOutcome,
}

/// What `wait` prints: how the Job ended, the exit code `wait` exits with and the output the Job printed.
#[derive(Debug, Serialize)]
pub(crate) struct Wait<'a> {
    pub(crate) job: &'a str,
    pub(crate) exit_code: u8,
    pub(crate) end: JobEnd,
    pub(crate) output: &'a str,
}

/// What `logs` prints: the Job or Service asked for and its recent output.
#[derive(Debug, Serialize)]
pub(crate) struct Logs<'a> {
    pub(crate) name: &'a str,
    pub(crate) output: &'a str,
}

/// What `up` prints, told apart by `result`.
#[derive(Debug, Serialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub(crate) enum Up<'a> {
    /// Every Service asked for is healthy, in the order `up` brought them up.
    Healthy { services: Vec<Reached<'a>> },
    /// The Service ended before it became healthy, or never did.
    Unhealthy {
        service: &'a str,
        needed_by: Option<&'a str>,
        end: ServiceEnd,
        /// The last lines of the output of the Service, as many as the text on stderr prints.
        output: &'a str,
    },
    /// What the Service needs is held outside Procly.
    Conflict {
        service: &'a str,
        needed_by: Option<&'a str>,
        conflicts: &'a [Conflict],
    },
}

/// A healthy Service `up` brought up or found running, where to reach it and the Lease it listens on.
#[derive(Debug, Serialize)]
pub(crate) struct Reached<'a> {
    service: &'a str,
    outcome: StartOutcome,
    via: Option<&'a str>,
    port: Option<u16>,
    url: Option<&'a str>,
    /// Every Lease of the Service, by Port name.
    ports: &'a BTreeMap<String, u16>,
    /// Where to reach each Lease whose scheme Procly knows, by Port name.
    urls: &'a BTreeMap<String, String>,
    /// The Service that needs this one, when `up` was asked for that one instead.
    needed_by: Option<&'a str>,
}

impl<'a> Reached<'a> {
    /// The Service `up` reached, with the Service that needs it when `up` was asked for that one instead.
    pub(crate) fn new(up: &'a UpService, needed_by: Option<&'a str>) -> Self {
        Self {
            service: &up.service,
            outcome: up.outcome,
            via: up.via.as_deref(),
            port: up.port,
            url: up.url.as_deref(),
            ports: &up.ports,
            urls: &up.urls,
            needed_by,
        }
    }
}

/// What `env` prints: the variables Procly injects into a Service.
#[derive(Debug, Serialize)]
pub(crate) struct Env<'a> {
    pub(crate) service: &'a str,
    pub(crate) variables: Variables<'a>,
}

/// Variables as a JSON object, in the order `env` exports them.
#[derive(Debug)]
pub(crate) struct Variables<'a>(pub(crate) &'a [(String, String)]);

impl Serialize for Variables<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_map(self.0.iter().map(|(name, value)| (name, value)))
    }
}

/// What `worktree release` prints: the Worktree, then what it stopped, what it released and what it kept.
#[derive(Debug, Serialize)]
pub(crate) struct WorktreeRelease<'a> {
    worktree: &'a Path,
    stopped: ReleaseStopped<'a>,
    released: ReleaseReleased<'a>,
    kept: ReleaseKept<'a>,
}

/// The Services and Jobs a Worktree release stopped.
#[derive(Debug, Serialize)]
struct ReleaseStopped<'a> {
    services: &'a [String],
    jobs: &'a [String],
}

/// The Compose services a Worktree release let go of and the Leases it released.
#[derive(Debug, Serialize)]
struct ReleaseReleased<'a> {
    compose_services: &'a [Released],
    leases: &'a [LeaseHeld],
}

/// The Leases and Logical resources a Worktree release kept.
#[derive(Debug, Serialize)]
struct ReleaseKept<'a> {
    leases: &'a [KeptLease],
    logical_resources: &'a [KeptResource],
}

impl<'a> WorktreeRelease<'a> {
    pub(crate) fn new(report: &'a ReleaseReport) -> Self {
        Self {
            worktree: &report.worktree,
            stopped: ReleaseStopped {
                services: &report.services,
                jobs: &report.jobs,
            },
            released: ReleaseReleased {
                compose_services: &report.compose_services,
                leases: &report.leases,
            },
            kept: ReleaseKept {
                leases: &report.kept_leases,
                logical_resources: &report.logical_resources,
            },
        }
    }
}

/// What `doctor` prints: whether every check passed, then each check.
#[derive(Debug, Serialize)]
pub(crate) struct Doctor<'a> {
    result: &'static str,
    checks: &'a [Check],
    /// What to tell a coding agent to fix the problems, `None` when no check failed or when it was handed to the
    /// `agent`.
    prompt: Option<&'a str>,
    /// The coding agent CLI the prompt was handed to.
    agent: Option<&'a str>,
}

impl<'a> Doctor<'a> {
    pub(crate) fn new(report: &'a Report, prompt: Option<&'a str>, agent: Option<&'a str>) -> Self {
        Self {
            result: if report.failed() { "problems" } else { "healthy" },
            checks: &report.checks,
            prompt,
            agent,
        }
    }
}

/// What `down` prints: the Service asked for, if any, the Services it stopped and the Compose services it let go of.
#[derive(Debug, Serialize)]
pub(crate) struct Down<'a> {
    pub(crate) service: Option<&'a str>,
    pub(crate) stopped: &'a [String],
    pub(crate) released: &'a [Released],
}

/// What `worktree init` prints, told apart by `result`.
#[derive(Debug, Serialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub(crate) enum WorktreeInit<'a> {
    /// The Worktree is ready.
    Ready {
        #[serde(flatten)]
        report: &'a InitReport,
    },
    /// A setup command failed, and the ones after it were not run.
    SetupFailed {
        /// What was copied and what ran before the command failed.
        #[serde(flatten)]
        report: &'a InitReport,
        command: &'a str,
        end: JobEnd,
        /// The last lines of the output of the command, as many as the text on stderr prints.
        output: &'a str,
    },
}

/// What `eject` prints: what it stopped, removed and left, and the prompt for what was left.
#[derive(Debug, Serialize)]
pub(crate) struct Eject<'a> {
    pub(crate) stopped: &'a [Stopped],
    pub(crate) removed: &'a [Removed],
    pub(crate) left: &'a [Left],
    /// What to undo in the `left` items, `None` when nothing was left or when it was handed to the `agent`.
    pub(crate) prompt: Option<&'a str>,
    /// The coding agent CLI the prompt was handed to.
    pub(crate) agent: Option<&'a str>,
}

/// What `unit install` prints: the Session unit it wrote.
#[derive(Debug, Serialize)]
pub(crate) struct UnitInstalled<'a> {
    pub(crate) path: &'a Path,
    /// Always set, the key `unit inspect` reports it under.
    pub(crate) installed: bool,
}

/// What `unit inspect` prints.
#[derive(Debug, Serialize)]
pub(crate) struct UnitInspection<'a> {
    pub(crate) path: &'a Path,
    pub(crate) installed: bool,
    /// Whether the session manager starts the Daemon with the session.
    pub(crate) starts_with_session: bool,
    /// Whether the Daemon of the unit runs now.
    pub(crate) daemon_running: bool,
}

/// What `unit remove` prints: the Session unit and whether it was installed, hence removed.
#[derive(Debug, Serialize)]
pub(crate) struct UnitRemoved<'a> {
    pub(crate) path: &'a Path,
    pub(crate) removed: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{BlockOutcome, Held, Occupant, StatusReport, Written};

    fn json<T: Serialize>(value: &T) -> String {
        serde_json::to_string(value).expect("a protocol type serializes")
    }

    #[test]
    fn a_document_is_one_line_that_starts_with_the_schema_version() {
        assert_eq!(
            render(&Logs {
                name: "web",
                output: "a\n"
            })
            .unwrap(),
            "{\"schema_version\":1,\"name\":\"web\",\"output\":\"a\\n\"}\n"
        );
    }

    #[test]
    fn the_status_document_starts_with_the_schema_version_then_lists_repositories_and_jobs() {
        assert_eq!(
            render(&StatusReport::default()).unwrap(),
            "{\"schema_version\":1,\"repositories\":[],\"jobs_outside_repositories\":[]}\n"
        );
    }

    #[test]
    fn variables_keep_the_order_env_exports_them_in() {
        let variables = vec![
            ("PORT".to_owned(), "3000".to_owned()),
            ("A_FIRST".to_owned(), "x".to_owned()),
        ];
        assert_eq!(
            serde_json::to_string(&Variables(&variables)).unwrap(),
            r#"{"PORT":"3000","A_FIRST":"x"}"#
        );
    }

    /// The documents embed these protocol types as they are, so their JSON is pinned here: a change to one is a
    /// change to the schema that bumps `SCHEMA_VERSION`, whatever the protocol does.
    #[test]
    fn the_protocol_types_a_document_embeds_serialize_as_documented() {
        assert_eq!(json(&JobEnd::Exited { code: 3 }), r#"{"kind":"exited","code":3}"#);
        assert_eq!(
            json(&JobEnd::Signaled { signal: 15 }),
            r#"{"kind":"signaled","signal":15}"#
        );
        assert_eq!(
            json(&JobEnd::TimedOut { timeout_ms: 1000 }),
            r#"{"kind":"timed_out","timeout_ms":1000}"#
        );
        for (end, expected) in [
            (ServiceEnd::Stopped, r#"{"kind":"stopped"}"#),
            (ServiceEnd::Lost, r#"{"kind":"lost"}"#),
            (ServiceEnd::Idle { timeout_ms: 5 }, r#"{"kind":"idle","timeout_ms":5}"#),
            (ServiceEnd::Failed { restarts: 2 }, r#"{"kind":"failed","restarts":2}"#),
            (
                ServiceEnd::HealthTimeout { timeout_ms: 5 },
                r#"{"kind":"health_timeout","timeout_ms":5}"#,
            ),
            (
                ServiceEnd::PortMismatch { leased: 1, actual: 2 },
                r#"{"kind":"port_mismatch","leased":1,"actual":2}"#,
            ),
            (
                ServiceEnd::Orphaned { pgid: 7, port: None },
                r#"{"kind":"orphaned","pgid":7,"port":null}"#,
            ),
            (
                ServiceEnd::NotListening { port: 9 },
                r#"{"kind":"not_listening","port":9}"#,
            ),
            (ServiceEnd::Exited { code: 1 }, r#"{"kind":"exited","code":1}"#),
            (ServiceEnd::Signaled { signal: 9 }, r#"{"kind":"signaled","signal":9}"#),
        ] {
            assert_eq!(json(&end), expected);
        }
        assert_eq!(json(&Held::Port { port: 5432 }), r#"{"kind":"port","port":5432}"#);
        assert_eq!(
            json(&Held::ContainerName { name: "db".to_owned() }),
            r#"{"kind":"container_name","name":"db"}"#
        );
        assert_eq!(
            json(&Held::Service {
                ports: vec![1],
                leased: vec![2]
            }),
            r#"{"kind":"service","ports":[1],"leased":[2]}"#
        );
        assert_eq!(
            json(&Occupant::Process {
                pid: 4,
                command: "node".to_owned()
            }),
            r#"{"kind":"process","pid":4,"command":"node"}"#
        );
        assert_eq!(
            json(&Occupant::Unknown {
                find: "lsof".to_owned()
            }),
            r#"{"kind":"unknown","find":"lsof"}"#
        );
        assert_eq!(json(&Written::EnvFile), r#""env_file""#);
        assert_eq!(json(&BlockOutcome::Edited), r#""edited""#);
        assert_eq!(json(&StartOutcome::Reused), r#""reused""#);
    }
}
