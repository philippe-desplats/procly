//! The versioned request and response protocol spoken over the Daemon socket.
//!
//! One connection carries one request and one response, each a single line of JSON.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::topology::Worktree;

/// Bumped on any incompatible change, the Daemon refuses a request carrying another version.
pub(crate) const PROTOCOL_VERSION: u32 = 17;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "request", rename_all = "snake_case")]
pub(crate) enum Request {
    /// Starts a Job and answers as soon as it is spawned, or answers with the running Job of the same Fingerprint.
    Run {
        command: String,
        cwd: PathBuf,
        /// The environment of the requesting command, so the Job sees the caller's `PATH` and not the Daemon's.
        env: Vec<(String, String)>,
        timeout_ms: u64,
    },
    /// Answers once the Job has ended.
    Wait { job: String },
    /// Answers the recent output of the Job `name`, or else of the Service `name` of the Worktree holding `cwd`. The
    /// output of a Compose service is the one of its container, read with `docker compose` in `env`.
    Logs {
        name: String,
        cwd: PathBuf,
        /// The environment of the requesting command, as for `Up`.
        env: Vec<(String, String)>,
    },
    /// Starts a declared Service of the Worktree holding `cwd`, or finds it running, and answers once it is healthy.
    /// Without `service`, starts every declared Service that no Group covers.
    Up {
        service: Option<String>,
        cwd: PathBuf,
        /// The environment of the requesting command, as for `Run`.
        env: Vec<(String, String)>,
        /// Writes the exclusion of the generated files to the `.gitignore` of the Worktree and not to the shared git
        /// exclude file, unless a block of Procly already sits in the `.gitignore`.
        gitignore: bool,
    },
    /// The variables Procly injects into the Service `service` of the Worktree holding `cwd`, without starting it.
    Env { service: String, cwd: PathBuf },
    /// Stops a Service of the Worktree holding `cwd`, or every Service of it without `service`, and answers once
    /// their trees are gone.
    Down { service: Option<String>, cwd: PathBuf },
    /// The Topology and the Jobs of the Worktree holding `cwd`, or of every Worktree with `all`.
    Status { cwd: PathBuf, all: bool },
    /// Prepares the Worktree holding `path`: copies the listed files from the Main worktree, then runs the setup
    /// commands it has not run yet, and answers once they have ended.
    WorktreeInit {
        path: PathBuf,
        /// The environment of the requesting command, handed to the setup commands as for `Run`.
        env: Vec<(String, String)>,
    },
    /// Writes the Rules block into the instruction file of the Worktree holding `cwd`, and the Guard into its Claude
    /// Code settings.
    AgentInstall {
        cwd: PathBuf,
        /// The instruction file for the Rules block, inside the Worktree. Without it, `AGENTS.md` or `CLAUDE.md`.
        file: Option<PathBuf>,
    },
    /// Makes the Guards and Rules blocks of the Repository holding `cwd` stand aside, or stand again with `off` unset.
    Switch { cwd: PathBuf, off: bool },
    /// Asks whether the Guard of the Repository holding `cwd` stands, once it met a command it knows.
    Guard { cwd: PathBuf },
    /// Stops the Services of the Repository holding `cwd` and removes what Procly wrote into it, apart from the
    /// Managed blocks that were edited.
    Eject { cwd: PathBuf },
    /// Lists the Orphans of every Repository, and deletes them with `apply`. `cwd` only names the requester in the
    /// Daemon log.
    Gc { cwd: PathBuf, apply: bool },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "response", rename_all = "snake_case")]
pub(crate) enum Response {
    Run {
        job: String,
        outcome: StartOutcome,
    },
    Ended {
        output: String,
        end: JobEnd,
    },
    Logs {
        output: String,
    },
    Status(StatusReport),
    /// The Service is healthy.
    Up {
        service: String,
        outcome: StartOutcome,
        port: Option<u16>,
        url: Option<String>,
        /// Every Lease of the Service, by Port name.
        ports: BTreeMap<String, u16>,
        /// The Group running it, when the Service was covered by a Group that runs and nothing was started.
        via: Option<String>,
        /// The Services it needs, in the order `up` brought them up.
        needs: Vec<UpService>,
        /// Where to reach each Lease whose scheme Procly knows, by Port name.
        urls: BTreeMap<String, String>,
    },
    /// Every Service `up` without a Service brought up or found running, in the order it brought them up.
    UpAll {
        services: Vec<UpService>,
    },
    /// The Service ended before it became healthy.
    Unhealthy {
        service: String,
        outcome: StartOutcome,
        end: ServiceEnd,
        /// The recent output of the Service, to tell why.
        output: String,
        /// The Service that needs this one, when `up` was asked for that one instead.
        needed_by: Option<String>,
    },
    /// The variables Procly injects into the Service, by name.
    Env {
        variables: Vec<(String, String)>,
    },
    /// The Services `down` stopped, by name, and the Compose services it let go of.
    Down {
        stopped: Vec<String>,
        released: Vec<Released>,
    },
    /// The Worktree is prepared.
    WorktreeInit(InitReport),
    /// A setup command is still running: sent as often as `settings.setup_progress_interval` while `worktree init`
    /// waits, before its final answer.
    SetupProgress {
        command: String,
        /// How long the command has run, in milliseconds.
        elapsed_ms: u64,
        /// The file the output of the setup commands is appended to, which outlives a failure.
        log: PathBuf,
    },
    /// A setup command failed, and the ones after it were not run.
    SetupFailed {
        report: InitReport,
        command: String,
        end: JobEnd,
        /// The recent output of the command, to tell why.
        output: String,
    },
    /// The Rules block and the Guard are written, or were already.
    AgentInstall(InstallReport),
    /// The Repository is Off or On, and its Rules blocks say so.
    Switched(SwitchReport),
    /// Whether the Guard stands for the Repository, and the Services it declares for the Worktree.
    Guard {
        off: bool,
        /// The directory asked about, relative to the root of its Worktree (empty at the root, where a dev command of
        /// the whole Repository runs), `None` when it cannot be placed.
        directory: Option<PathBuf>,
        /// The root of the Worktree, which a compose file given with an absolute path is placed against.
        root: PathBuf,
        services: Vec<GuardService>,
    },
    /// The Repository is ejected.
    Eject(EjectReport),
    /// The Orphans `gc` found, and what became of each with `apply`.
    Gc(GcReport),
    Error {
        message: String,
    },
    /// What the Service needs is held outside Procly, which never stops it.
    Conflict {
        service: String,
        conflicts: Vec<Conflict>,
        /// The Service that needs this one, when `up` was asked for that one instead.
        needed_by: Option<String>,
    },
}

/// Something started outside Procly that holds what a Service needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Conflict {
    /// What the Service needs and the occupant holds.
    pub(crate) held: Held,
    /// What holds it.
    pub(crate) occupant: Occupant,
    /// The command that frees what the Service needs, which Procly never runs itself. `None` when the occupant
    /// cannot be named.
    pub(crate) stop: Option<String>,
}

/// What an occupant holds that a Service needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Held {
    /// The port of a Lease of the Service.
    Port { port: u16 },
    /// The name the container of the Compose service gets.
    ContainerName { name: String },
    /// The Compose service itself, whose container runs with matching labels but publishes `ports` instead of the
    /// Leases `leased`.
    Service { ports: Vec<u16>, leased: Vec<u16> },
}

/// What holds a port or a name, as Docker and `lsof` tell it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Occupant {
    /// A container, with its compose labels when it has them.
    Container {
        name: String,
        project: Option<String>,
        service: Option<String>,
    },
    /// A process outside any container, `command` being its name as `lsof` prints it.
    Process { pid: i32, command: String },
    /// Something answers on the port that neither Docker nor `lsof` can name, such as a process of another user,
    /// with the command that finds it with more rights.
    Unknown { find: String },
}

/// A dev process or container running in a Worktree that Procly did not start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum ForeignProcess {
    /// A process listening on TCP ports, whose working directory is in the Worktree.
    Process { pid: i32, command: String, ports: Vec<u16> },
    /// A running container of a compose project whose directory is in the Worktree.
    Container {
        name: String,
        project: String,
        service: String,
        ports: Vec<u16>,
    },
}

/// What a request to write a Managed block did to the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BlockOutcome {
    /// The block was added or updated.
    Written,
    /// The file already held exactly this block.
    Unchanged,
    /// The block holds something Procly did not write, so it was left as it is.
    Edited,
}

impl BlockOutcome {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Written => "written",
            Self::Unchanged => "unchanged",
            Self::Edited => "edited",
        }
    }
}

/// A Managed block and what became of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BlockReport {
    pub(crate) file: PathBuf,
    pub(crate) outcome: BlockOutcome,
}

/// What `agent install` wrote.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct InstallReport {
    pub(crate) rules: BlockReport,
    pub(crate) guard: BlockReport,
    /// Whether the Repository is Off, in which case the Rules block says so.
    pub(crate) off: bool,
}

/// What `off` and `on` did to the Rules blocks of the Repository.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct SwitchReport {
    pub(crate) off: bool,
    pub(crate) rules: Vec<BlockReport>,
}

/// What kind of file or text Procly wrote into a Repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Written {
    /// The Rules block of an instruction file.
    Rules,
    /// The Guard of the Claude Code settings.
    Guard,
    /// The Managed block excluding the generated files from git.
    Exclusions,
    /// The Env file of a Worktree.
    EnvFile,
}

impl Written {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Rules => "rules",
            Self::Guard => "guard",
            Self::Exclusions => "exclusions",
            Self::EnvFile => "env file",
        }
    }
}

/// Something `eject` removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Removed {
    pub(crate) kind: Written,
    pub(crate) file: PathBuf,
    /// Whether the file held nothing else, so that it was deleted with it.
    pub(crate) deleted: bool,
}

/// Something `eject` left as it is because the developer edited it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Left {
    pub(crate) kind: Written,
    pub(crate) file: PathBuf,
}

/// A Service `eject` stopped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Stopped {
    pub(crate) service: String,
    /// The Worktree it ran for, `None` for a Compose service whose container belongs to the Repository.
    pub(crate) worktree: Option<PathBuf>,
    /// Whether it is a Compose service whose container belongs to `worktree`, an Isolated one. Left out when not, so
    /// that the report of the other Services reads as it did before Isolated ones.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) isolated: bool,
}

/// What `eject` did to a Repository.
#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct EjectReport {
    pub(crate) stopped: Vec<Stopped>,
    pub(crate) removed: Vec<Removed>,
    pub(crate) left: Vec<Left>,
    /// What to undo in the `left` items, written for a coding agent. `None` when nothing was left.
    pub(crate) prompt: Option<String>,
}

/// A healthy Service `up` brought up or found running.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct UpService {
    pub(crate) service: String,
    pub(crate) outcome: StartOutcome,
    /// The Group running it, as for [`Response::Up`].
    pub(crate) via: Option<String>,
    pub(crate) port: Option<u16>,
    pub(crate) url: Option<String>,
    /// Every Lease of the Service, by Port name, the one `port` is the health check's among them.
    pub(crate) ports: BTreeMap<String, u16>,
    /// Where to reach each Lease whose scheme Procly knows, by Port name, the one `url` is among them.
    pub(crate) urls: BTreeMap<String, String>,
}

/// A Compose service a Worktree let go of, whose container keeps running while another Worktree needs it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Released {
    pub(crate) service: String,
    /// How many other Worktrees still need the container.
    pub(crate) others: usize,
    /// How long the container keeps running once no Worktree needs it.
    pub(crate) grace_ms: u64,
}

/// Why a Service ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum ServiceEnd {
    Exited {
        code: i32,
    },
    Signaled {
        signal: i32,
    },
    /// The Service did not become healthy in time, and its whole process tree was stopped.
    HealthTimeout {
        timeout_ms: u64,
    },
    /// `down` stopped the Service.
    Stopped,
    /// The Service printed nothing and used almost no CPU for `timeout_ms`, and was stopped.
    Idle {
        timeout_ms: u64,
    },
    /// The Service crashed again after being restarted `restarts` times, which is as often as it may be.
    Failed {
        restarts: u32,
    },
    /// The process group of the Service listened on `actual` and not on its Lease `leased`, and its whole process
    /// tree was stopped.
    PortMismatch {
        leased: u16,
        actual: u16,
    },
    /// A previous Daemon started the Service, and its process ended without the Daemon of now being able to wait
    /// for it, so its exit code is unknown.
    Lost,
    /// The leader of the process group had ended when the Daemon restarted, and the rest of the group, which held
    /// the leased `port` when it is set, was stopped.
    Orphaned {
        pgid: i32,
        port: Option<u16>,
    },
    /// The process group still ran when the Daemon restarted, but no longer listened on its Lease `port`, so it was
    /// stopped instead of being re-attached.
    NotListening {
        port: u16,
    },
}

/// Whether a start request started new work or ended in Reuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum StartOutcome {
    Started,
    Reused,
}

impl StartOutcome {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Reused => "reused",
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct StatusReport {
    pub(crate) repositories: Vec<RepositoryStatus>,
    /// Jobs started in a directory outside any Repository.
    pub(crate) jobs_outside_repositories: Vec<JobStatus>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct RepositoryStatus {
    pub(crate) git_common_dir: PathBuf,
    pub(crate) worktrees: Vec<WorktreeStatus>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct WorktreeStatus {
    #[serde(flatten)]
    pub(crate) worktree: Worktree,
    pub(crate) services: Vec<ServiceStatus>,
    pub(crate) jobs: Vec<JobStatus>,
    /// What runs in the Worktree without Procly having started it. Left out when empty, so that the status of a
    /// Worktree without any reads as it did before Foreign processes were reported.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) foreign: Vec<ForeignProcess>,
    /// The Isolated containers Procly runs for the Worktree. Left out when empty, so that the status of a Worktree
    /// without any reads as it did before Isolated containers were reported.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) containers: Vec<ContainerStatus>,
}

/// An Isolated container Procly started or adopted for a Worktree, which runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ContainerStatus {
    /// The name of the Compose service in `procly.toml`.
    pub(crate) service: String,
    /// The compose project of the Worktree the container belongs to.
    pub(crate) project: String,
    /// The service of the compose file it runs.
    pub(crate) compose: String,
    /// The Lease its health check probes.
    pub(crate) port: Option<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ServiceStatus {
    pub(crate) service: String,
    pub(crate) port: Option<u16>,
    /// Whether a previous Daemon started the Service, and this one found it running when it started.
    pub(crate) reattached: bool,
    #[serde(flatten)]
    pub(crate) state: ServiceState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum ServiceState {
    Starting,
    Healthy,
    /// The process crashed and its restart is waiting out the delay.
    Restarting,
    Ended {
        end: ServiceEnd,
    },
}

/// A declared Service, as the Guard matches a command it blocks against it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct GuardService {
    pub(crate) name: String,
    /// The command of a Process service or a Group.
    pub(crate) run: Option<String>,
    /// The service of the compose file a Compose service maps to.
    pub(crate) compose: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct JobStatus {
    pub(crate) id: String,
    pub(crate) command: String,
    pub(crate) cwd: PathBuf,
    #[serde(flatten)]
    pub(crate) state: JobState,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum JobState {
    Running,
    Ended { end: JobEnd },
}

/// What `worktree init` changed in a Worktree.
#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct InitReport {
    /// The listed paths that were copied, as listed.
    pub(crate) copied: Vec<String>,
    /// The listed paths the Main worktree does not hold.
    pub(crate) missing: Vec<String>,
    /// The setup commands that ran and succeeded.
    pub(crate) ran: Vec<String>,
}

/// How a Job ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum JobEnd {
    Exited {
        code: i32,
    },
    Signaled {
        signal: i32,
    },
    /// The Job reached its timeout and its whole process tree was killed.
    TimedOut {
        timeout_ms: u64,
    },
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Envelope<T> {
    pub(crate) version: u32,
    #[serde(flatten)]
    pub(crate) body: T,
}

impl<T> Envelope<T> {
    pub(crate) fn new(body: T) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            body,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum DecodeError {
    #[error("malformed message: {0}")]
    Malformed(#[from] serde_json::Error),
    #[error("protocol version {0} is not supported, this procly speaks version {PROTOCOL_VERSION}")]
    Version(u32),
}

/// Decodes one line, checking the version before the body so that a newer peer gets a clear answer.
pub(crate) fn decode<T: for<'de> Deserialize<'de>>(line: &str) -> Result<T, DecodeError> {
    #[derive(Deserialize)]
    struct Version {
        version: u32,
    }
    let Version { version } = serde_json::from_str(line)?;
    if version != PROTOCOL_VERSION {
        return Err(DecodeError::Version(version));
    }
    let envelope: Envelope<T> = serde_json::from_str(line)?;
    Ok(envelope.body)
}

/// Encodes one message as a line, newline included. Fails only on a path that is not valid UTF-8.
pub(crate) fn encode<T: Serialize>(body: T) -> Result<String, serde_json::Error> {
    let mut line = serde_json::to_string(&Envelope::new(body))?;
    line.push('\n');
    Ok(line)
}

/// What `gc` found, and with `applied` what it deleted.
#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct GcReport {
    /// Whether `gc` was asked to delete the Orphans, and not only to list them.
    pub(crate) applied: bool,
    /// Every Orphan, Leases first, then Logical resources, then output directories, setup directories, Isolated
    /// containers and their volumes, the order they are deleted in.
    pub(crate) orphans: Vec<Orphan>,
    /// The Repositories whose Worktrees git could not list, nothing of which is an Orphan.
    pub(crate) skipped: Vec<SkippedRepository>,
}

impl GcReport {
    /// Whether some deletion failed.
    pub(crate) fn failed(&self) -> bool {
        self.orphans.iter().any(|orphan| orphan.error.is_some())
    }
}

/// Something a removed Worktree left behind, whose Worktree git no longer lists and that no Pinned branch holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Orphan {
    #[serde(flatten)]
    pub(crate) kind: OrphanKind,
    /// The common git directory identifying the Repository.
    pub(crate) repository: PathBuf,
    /// The root of the removed Worktree.
    pub(crate) worktree: PathBuf,
    /// Whether it was deleted, always `false` without `apply`.
    pub(crate) deleted: bool,
    /// Why it could not be deleted.
    pub(crate) error: Option<String>,
}

impl Orphan {
    /// What the Orphan is and which Worktree it belongs to, for the text of `gc` and the Daemon log.
    pub(crate) fn describe(&self) -> String {
        let worktree = self.worktree.display();
        match &self.kind {
            OrphanKind::Lease {
                service,
                port_name,
                port,
            } => format!("lease {service}.{port_name} port {port} of {worktree}"),
            OrphanKind::LogicalResource {
                service,
                resource,
                name,
                driver,
            } => format!("logical resource {service}.{resource} {name} (driver {driver}) of {worktree}"),
            OrphanKind::OutputDirectory { path } => format!("output directory {} of {worktree}", path.display()),
            OrphanKind::SetupDirectory { path } => format!("setup directory {} of {worktree}", path.display()),
            OrphanKind::IsolatedContainer {
                service,
                project,
                compose,
            } => format!("isolated container {service} (compose project {project}, service {compose}) of {worktree}"),
            OrphanKind::IsolatedVolume { project, volume } => {
                format!("isolated volume {volume} (compose project {project}) of {worktree}")
            }
        }
    }
}

/// What an Orphan is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum OrphanKind {
    /// A Lease of a Port name of a Service.
    Lease {
        service: String,
        port_name: String,
        port: u16,
    },
    /// A Logical resource of a Compose service, in its Logical container, dropped through the `drop` command of its
    /// Driver, which frees its index when the Driver numbers its resources.
    LogicalResource {
        service: String,
        /// The key of the resource in the configuration.
        resource: String,
        /// Its name in the container, its index for a Driver that numbers its resources.
        name: String,
        driver: String,
    },
    /// The directory of the state directory holding the output files of the Services of the Worktree.
    OutputDirectory { path: PathBuf },
    /// The directory of the state directory holding the log of the setup commands of the Worktree.
    SetupDirectory { path: PathBuf },
    /// The Isolated container of a Compose service, running in the compose project of the Worktree.
    IsolatedContainer {
        /// The name of the Service in `procly.toml`.
        service: String,
        project: String,
        /// The service of the compose file.
        compose: String,
    },
    /// A named volume of the compose project of the Worktree, which an Isolated container mounted.
    IsolatedVolume { project: String, volume: String },
}

/// A Repository whose Worktrees git could not list, so that none of them counts as removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SkippedRepository {
    /// The common git directory identifying the Repository.
    pub(crate) repository: PathBuf,
    /// Why git could not list its Worktrees.
    pub(crate) error: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_message_round_trips_through_a_line() {
        let line = encode(Request::Wait {
            job: "3f9a1c2e".to_owned(),
        })
        .unwrap();
        assert_eq!(line, "{\"version\":17,\"request\":\"wait\",\"job\":\"3f9a1c2e\"}\n");
        assert!(matches!(decode::<Request>(&line), Ok(Request::Wait { job }) if job == "3f9a1c2e"));
    }

    #[test]
    fn an_orphan_carries_its_kind_beside_its_worktree_and_round_trips() {
        let orphan = Orphan {
            kind: OrphanKind::Lease {
                service: "web".to_owned(),
                port_name: "http".to_owned(),
                port: 24000,
            },
            repository: PathBuf::from("/src/app/.git"),
            worktree: PathBuf::from("/src/app-a"),
            deleted: false,
            error: None,
        };
        let line = encode(Response::Gc(GcReport {
            applied: false,
            orphans: vec![orphan.clone()],
            skipped: Vec::new(),
        }))
        .unwrap();
        assert!(line.contains(
            r#"{"kind":"lease","service":"web","port_name":"http","port":24000,"repository":"/src/app/.git","#
        ));
        assert!(matches!(decode::<Response>(&line), Ok(Response::Gc(report)) if report.orphans == [orphan]));
    }

    #[test]
    fn a_path_that_is_not_utf8_fails_to_encode_instead_of_panicking() {
        use std::os::unix::ffi::OsStrExt as _;

        let cwd = PathBuf::from(std::ffi::OsStr::from_bytes(b"/src/caf\xe9"));
        assert!(encode(Request::Status { cwd, all: false }).is_err());
    }

    #[test]
    fn another_protocol_version_is_refused_before_its_body_is_read() {
        let line = r#"{"version":99,"request":"teleport"}"#;
        assert!(matches!(decode::<Request>(line), Err(DecodeError::Version(99))));
    }
}
