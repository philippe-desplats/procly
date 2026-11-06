//! The versioned request and response protocol spoken over the Daemon socket.
//!
//! One connection carries one request, then the responses to it, each a single line of JSON.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// A Worktree as the protocol carries it. Discovering it through `git` is the business of the CLI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct Worktree {
    /// The root directory of the Worktree, canonicalized.
    pub path: PathBuf,
    pub slug: String,
    /// Whether this is the Main worktree, the first entry of `git worktree list`.
    pub main: bool,
}

/// Bumped on any incompatible change, the Daemon refuses a request carrying another version.
pub const PROTOCOL_VERSION: u32 = 22;

#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(tag = "request", rename_all = "snake_case")]
pub enum Request {
    /// Starts a Job and answers as soon as it is spawned, or answers with the running Job of the same Fingerprint.
    Run {
        command: String,
        cwd: PathBuf,
        /// The environment of the requesting command, so the Job sees the caller's `PATH` and not the Daemon's.
        env: Vec<(String, String)>,
        timeout_ms: u64,
    },
    /// Answers once the Job has ended. An ended Job the Daemon no longer keeps, evicted by its bound or forgotten by a
    /// Worktree release, is an unknown Job and answers [`Response::Error`].
    Wait { job: String },
    /// Answers the recent output of the Job `name`, or else of the Service `name` of the Worktree holding `cwd`. An
    /// ended Job that was evicted is no longer found, as for [`Request::Wait`]. The
    /// output of a Compose service is the one of its container, read with `docker compose` in `env`.
    ///
    /// With `follow`, the answer is a stream: [`Response::Logs`] with the backlog, then [`Response::Output`] as output
    /// arrives, through the automatic restarts of a Service that crashed, then [`Response::LogsEnded`] once the Job
    /// or the Service has ended. A reader too slow to keep up gets [`Response::OutputDropped`] in place of what it
    /// missed, the Daemon never waits for it. The client sends nothing after its request, and cancels by closing the
    /// connection, or only its writing half. A Compose service cannot be followed, and answers [`Response::Error`]
    /// with the `docker compose logs -f` command to run instead.
    Logs {
        name: String,
        cwd: PathBuf,
        /// The environment of the requesting command, as for `Up`.
        env: Vec<(String, String)>,
        /// False when absent.
        #[serde(default)]
        follow: bool,
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
    /// Daemon log. With `only`, absent when not given, it is limited to the Orphans it lists: the Daemon deletes those
    /// that still are Orphans, and reports the others as not deleted, with the reason in their `error`.
    Gc {
        cwd: PathBuf,
        apply: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        only: Option<Vec<OrphanId>>,
    },
    /// Runs the Worktree release of the Worktree holding `path`: stops its Services and Jobs, lets go of its Compose
    /// services and releases its Leases, apart from those of a Pinned branch, and answers once their trees are gone.
    /// With `dry_run`, false when absent, it answers with what the release would do and changes nothing.
    WorktreeRelease {
        path: PathBuf,
        #[serde(default)]
        dry_run: bool,
    },
    /// Runs the checks only the Daemon can answer, and tells which version it is.
    Doctor,
    /// Writes the Worktrunk hooks block into the `.config/wt.toml` of the Worktree holding `cwd`, unless a hook is
    /// already set outside it.
    WorktreeHooks { cwd: PathBuf },
    /// Restarts the Service `service` of the Worktree holding `cwd` with the environment of its last `Up`, which the
    /// Daemon holds in memory, and answers as `Up` does for one Service. It stops the Service if it runs, starts the
    /// Services it needs that do not run, and restarts a Service a Group covers through its Group. Without a held
    /// environment, answers `Error`.
    Restart { service: String, cwd: PathBuf },
}

#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(tag = "response", rename_all = "snake_case")]
pub enum Response {
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
        /// What went wrong without failing `up`, such as a Portless alias that could not be made.
        warnings: Vec<String>,
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
    /// The Worktree is released.
    WorktreeRelease(ReleaseReport),
    /// What the Daemon found when it ran its checks.
    Doctor(DaemonChecks),
    /// What became of the Worktrunk hooks block.
    WorktreeHooks(HooksReport),
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
    /// Output the Job or the Service printed after the backlog of a followed [`Request::Logs`].
    Output {
        text: String,
    },
    /// Output a followed [`Request::Logs`] did not carry, as its reader was too slow to keep up.
    OutputDropped {
        bytes: u64,
    },
    /// The Job or the Service of a followed [`Request::Logs`] has ended, and every line of its output was sent.
    LogsEnded,
}

/// Something started outside Procly that holds what a Service needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct Conflict {
    /// What the Service needs and the occupant holds.
    pub held: Held,
    /// What holds it.
    pub occupant: Occupant,
    /// The command that frees what the Service needs, which Procly never runs itself. `None` when the occupant
    /// cannot be named.
    pub stop: Option<String>,
}

/// What an occupant holds that a Service needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Held {
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
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Occupant {
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
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ForeignProcess {
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
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum BlockOutcome {
    /// The block was added or updated.
    Written,
    /// The file already held exactly this block.
    Unchanged,
    /// The block holds something Procly did not write, so it was left as it is.
    Edited,
}

impl BlockOutcome {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Written => "written",
            Self::Unchanged => "unchanged",
            Self::Edited => "edited",
        }
    }
}

/// A Managed block and what became of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct BlockReport {
    pub file: PathBuf,
    pub outcome: BlockOutcome,
}

/// What `agent install` wrote.
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct InstallReport {
    pub rules: BlockReport,
    pub guard: BlockReport,
    /// Whether the Repository is Off, in which case the Rules block says so.
    pub off: bool,
}

/// What `off` and `on` did to the Rules blocks of the Repository.
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct SwitchReport {
    pub off: bool,
    pub rules: Vec<BlockReport>,
}

/// What kind of file or text Procly wrote into a Repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum Written {
    /// The Rules block of an instruction file.
    Rules,
    /// The Guard of the Claude Code settings.
    Guard,
    /// The Managed block excluding the generated files from git.
    Exclusions,
    /// The Env file of a Worktree.
    EnvFile,
    /// The Managed block of the Worktrunk config that runs `worktree init` and `worktree release`.
    WorktrunkHooks,
}

impl Written {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rules => "rules",
            Self::Guard => "guard",
            Self::Exclusions => "exclusions",
            Self::EnvFile => "env file",
            Self::WorktrunkHooks => "worktrunk hooks",
        }
    }
}

/// Something `eject` removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct Removed {
    pub kind: Written,
    pub file: PathBuf,
    /// Whether the file held nothing else, so that it was deleted with it.
    pub deleted: bool,
}

/// Something `eject` left as it is because the developer edited it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct Left {
    pub kind: Written,
    pub file: PathBuf,
}

/// A Service `eject` stopped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct Stopped {
    pub service: String,
    /// The Worktree it ran for, `None` for a Compose service whose container belongs to the Repository.
    pub worktree: Option<PathBuf>,
    /// Whether it is a Compose service whose container belongs to `worktree`, an Isolated one. Left out when not, so
    /// that the report of the other Services reads as it did before Isolated ones.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub isolated: bool,
}

/// What `eject` did to a Repository.
#[derive(Debug, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct EjectReport {
    pub stopped: Vec<Stopped>,
    pub removed: Vec<Removed>,
    pub left: Vec<Left>,
    /// What to undo in the `left` items, written for a coding agent. `None` when nothing was left.
    pub prompt: Option<String>,
}

/// A healthy Service `up` brought up or found running.
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct UpService {
    pub service: String,
    pub outcome: StartOutcome,
    /// The Group running it, as for [`Response::Up`].
    pub via: Option<String>,
    pub port: Option<u16>,
    pub url: Option<String>,
    /// Every Lease of the Service, by Port name, the one `port` is the health check's among them.
    pub ports: BTreeMap<String, u16>,
    /// Where to reach each Lease whose scheme Procly knows, by Port name, the one `url` is among them.
    pub urls: BTreeMap<String, String>,
    /// What went wrong without failing `up`, such as a Portless alias that could not be made.
    pub warnings: Vec<String>,
}

/// A Compose service a Worktree let go of, whose container keeps running while another Worktree needs it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct Released {
    pub service: String,
    /// How many other Worktrees still need the container.
    pub others: usize,
    /// How long the container keeps running once no Worktree needs it.
    pub grace_ms: u64,
}

/// What a Worktree release did to a Worktree.
#[derive(Debug, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct ReleaseReport {
    pub worktree: PathBuf,
    /// The Services it stopped, by name.
    pub services: Vec<String>,
    /// The Jobs it stopped, by identifier.
    pub jobs: Vec<String>,
    /// The Compose services the Worktree let go of.
    pub compose_services: Vec<Released>,
    /// The Leases it released, whose ports any Worktree can be granted now.
    pub leases: Vec<LeaseHeld>,
    /// The Leases of the Worktree it kept.
    pub kept_leases: Vec<KeptLease>,
    /// The Logical resources of the Worktree, which a Worktree release never drops.
    pub logical_resources: Vec<KeptResource>,
    /// What went wrong without failing the release, such as a Portless alias that could not be removed.
    pub warnings: Vec<String>,
}

impl ReleaseReport {
    /// Whether nothing was stopped, let go of nor released, as for a second release or a Worktree Procly never saw.
    #[must_use]
    pub fn released_nothing(&self) -> bool {
        self.services.is_empty() && self.jobs.is_empty() && self.compose_services.is_empty() && self.leases.is_empty()
    }
}

/// The port one Port name of one Service holds through its Lease.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct LeaseHeld {
    pub service: String,
    pub port_name: String,
    pub port: u16,
}

/// A Lease a Worktree release kept, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct KeptLease {
    #[serde(flatten)]
    pub lease: LeaseHeld,
    #[serde(flatten)]
    pub reason: KeptBecause,
}

/// Why a Worktree release kept a Lease.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum KeptBecause {
    /// The Lease is bound to the Pinned branch `branch` the Worktree is on, so that it survives the Worktree.
    PinnedBranch { branch: String },
    /// The container of the Compose service still runs on it, for the other Worktrees or its Grace period.
    ContainerRunning,
}

/// A Logical resource a Worktree release kept.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct KeptResource {
    /// The Compose service whose container holds it.
    pub service: String,
    pub driver: String,
    /// The key of the resource in the configuration.
    pub resource: String,
    /// Its name in the container.
    pub name: String,
}

/// Why a Service ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ServiceEnd {
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
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum StartOutcome {
    Started,
    Reused,
}

impl StartOutcome {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Reused => "reused",
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct StatusReport {
    pub repositories: Vec<RepositoryStatus>,
    /// Jobs started in a directory outside any Repository.
    pub jobs_outside_repositories: Vec<JobStatus>,
}

#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct RepositoryStatus {
    pub git_common_dir: PathBuf,
    pub worktrees: Vec<WorktreeStatus>,
}

#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct WorktreeStatus {
    #[serde(flatten)]
    pub worktree: Worktree,
    /// The branch checked out in the Worktree, as the short name git lists it under. Left out on a detached `HEAD`,
    /// for a Worktree git no longer lists, and in a Repository without a branch to report.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Whether `git worktree list` still lists the Worktree. A Worktree the Daemon ran something in, and that git
    /// no longer lists, is reported with `false`.
    pub listed: bool,
    pub services: Vec<ServiceStatus>,
    pub jobs: Vec<JobStatus>,
    /// What runs in the Worktree without Procly having started it. Left out when empty, so that the status of a
    /// Worktree without any reads as it did before Foreign processes were reported.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub foreign: Vec<ForeignProcess>,
    /// The Isolated containers Procly runs for the Worktree. Left out when empty, so that the status of a Worktree
    /// without any reads as it did before Isolated containers were reported.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub containers: Vec<ContainerStatus>,
}

/// An Isolated container Procly started or adopted for a Worktree, which runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct ContainerStatus {
    /// The name of the Compose service in `procly.toml`.
    pub service: String,
    /// The compose project of the Worktree the container belongs to.
    pub project: String,
    /// The service of the compose file it runs.
    pub compose: String,
    /// The Lease its health check probes.
    pub port: Option<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct ServiceStatus {
    pub service: String,
    pub port: Option<u16>,
    /// Whether a previous Daemon started the Service, and this one found it running when it started.
    pub reattached: bool,
    /// When the Service last started, in milliseconds since the Unix epoch. An automatic restart sets it again. For
    /// a Service a previous Daemon started, it is when this Daemon found it running.
    pub started_at: u64,
    /// Whether the Daemon holds the environment of the last `up` of the Service, which a `Restart` replays. The
    /// environment itself is never part of the status.
    pub restartable: bool,
    #[serde(flatten)]
    pub state: ServiceState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ServiceState {
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
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct GuardService {
    pub name: String,
    /// The command of a Process service or a Group.
    pub run: Option<String>,
    /// The service of the compose file a Compose service maps to.
    pub compose: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct JobStatus {
    pub id: String,
    pub command: String,
    pub cwd: PathBuf,
    #[serde(flatten)]
    pub state: JobState,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum JobState {
    Running,
    Ended { end: JobEnd },
}

/// What `worktree init` changed in a Worktree.
#[derive(Debug, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct InitReport {
    /// The listed paths that were copied, as listed.
    pub copied: Vec<String>,
    /// The listed paths the Main worktree does not hold.
    pub missing: Vec<String>,
    /// The setup commands that ran and succeeded.
    pub ran: Vec<String>,
}

/// How a Job ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JobEnd {
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
pub struct Envelope<T> {
    pub version: u32,
    #[serde(flatten)]
    pub body: T,
}

impl<T> Envelope<T> {
    pub fn new(body: T) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            body,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("malformed message: {0}")]
    Malformed(#[from] serde_json::Error),
    #[error("protocol version {0} is not supported, this procly speaks version {PROTOCOL_VERSION}")]
    Version(u32),
}

/// Decodes one line, checking the version before the body so that a newer peer gets a clear answer.
///
/// # Errors
///
/// [`DecodeError::Version`] when the line carries another protocol version, [`DecodeError::Malformed`] when it is not
/// a message of this one.
pub fn decode<T: for<'de> Deserialize<'de>>(line: &str) -> Result<T, DecodeError> {
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

/// Encodes one message as a line, newline included.
///
/// # Errors
///
/// Only on a path that is not valid UTF-8.
pub fn encode<T: Serialize>(body: T) -> Result<String, serde_json::Error> {
    let mut line = serde_json::to_string(&Envelope::new(body))?;
    line.push('\n');
    Ok(line)
}

/// What `gc` found, and with `applied` what it deleted.
#[derive(Debug, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct GcReport {
    /// Whether `gc` was asked to delete the Orphans, and not only to list them.
    pub applied: bool,
    /// Every Orphan, Leases first, then Logical resources, then output directories, setup directories, Isolated
    /// containers and their volumes, the order they are deleted in.
    pub orphans: Vec<Orphan>,
    /// The Repositories whose Worktrees git could not list, nothing of which is an Orphan.
    pub skipped: Vec<SkippedRepository>,
    /// What went wrong without failing `gc`, such as a Portless alias that could not be removed. One for all the
    /// Orphans, not one for each.
    pub warnings: Vec<String>,
}

impl GcReport {
    /// Whether some deletion failed.
    #[must_use]
    pub fn failed(&self) -> bool {
        self.orphans.iter().any(|orphan| orphan.error.is_some())
    }
}

/// What tells an Orphan from any other: what it is and the Worktree it belongs to. An [`Orphan`] carries it flattened,
/// so that the `orphans` of a dry run, sent back as returned, deserialize as the `only` of [`Request::Gc`], serde
/// ignoring `deleted` and `error`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct OrphanId {
    #[serde(flatten)]
    pub kind: OrphanKind,
    /// The common git directory identifying the Repository.
    pub repository: PathBuf,
    /// The root of the removed Worktree.
    pub worktree: PathBuf,
}

/// Something a removed Worktree left behind, whose Worktree git no longer lists and that no Pinned branch holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct Orphan {
    #[serde(flatten)]
    pub id: OrphanId,
    /// Whether it was deleted, always `false` without `apply`.
    pub deleted: bool,
    /// Why it could not be deleted.
    pub error: Option<String>,
}

impl Orphan {
    /// What the Orphan is and which Worktree it belongs to, for the text of `gc` and the Daemon log.
    #[must_use]
    pub fn describe(&self) -> String {
        let worktree = self.id.worktree.display();
        match &self.id.kind {
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
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OrphanKind {
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
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct SkippedRepository {
    /// The common git directory identifying the Repository.
    pub repository: PathBuf,
    /// Why git could not list its Worktrees.
    pub error: String,
}

/// What a `doctor` check found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    /// Nothing is wrong.
    Passed,
    /// A problem, which makes `doctor` exit with 7.
    Failed,
    /// The check does not apply here, or could not run.
    Skipped,
    /// Something is missing that Procly can do without, so `doctor` still exits with 0.
    Warning,
}

impl CheckStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
            Self::Warning => "warning",
        }
    }
}

/// One check of `procly doctor`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct Check {
    /// What is checked, the same in every run so that a script can match it.
    pub name: String,
    pub status: CheckStatus,
    pub message: String,
    /// The command that fixes the problem, which `doctor` never runs. `None` when nothing is wrong, or when no command
    /// fixes it.
    pub fix: Option<String>,
}

impl Check {
    /// A check that found nothing wrong.
    pub fn passed(name: &str, message: impl Into<String>) -> Self {
        Self {
            name: name.to_owned(),
            status: CheckStatus::Passed,
            message: message.into(),
            fix: None,
        }
    }

    /// A problem, with the command that fixes it when one does.
    pub fn failed(name: &str, message: impl Into<String>, fix: Option<String>) -> Self {
        Self {
            name: name.to_owned(),
            status: CheckStatus::Failed,
            message: message.into(),
            fix,
        }
    }

    /// Something missing that Procly can do without, with the command that adds it, which `doctor` never runs.
    pub fn warning(name: &str, message: impl Into<String>, fix: Option<String>) -> Self {
        Self {
            name: name.to_owned(),
            status: CheckStatus::Warning,
            message: message.into(),
            fix,
        }
    }

    /// A check that does not apply here or could not run, `why` telling which.
    pub fn skipped(name: &str, why: impl Into<String>) -> Self {
        Self {
            name: name.to_owned(),
            status: CheckStatus::Skipped,
            message: why.into(),
            fix: None,
        }
    }
}

/// What `worktree hooks` did to the Worktrunk config.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum HooksOutcome {
    /// The block was added or updated.
    Written,
    /// The file already held exactly this block.
    Unchanged,
    /// The block holds something Procly did not write, so it was left as it is.
    Edited,
    /// A hook is already set outside the block, so nothing was written.
    Conflict,
}

impl HooksOutcome {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Written => "written",
            Self::Unchanged => "unchanged",
            Self::Edited => "edited",
            Self::Conflict => "conflict",
        }
    }
}

/// What `worktree hooks` wrote, or why it wrote nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct HooksReport {
    /// The `.config/wt.toml` of the Worktree.
    pub file: PathBuf,
    pub outcome: HooksOutcome,
    /// The hooks already set outside the block, for a `conflict`, empty otherwise.
    pub set: Vec<String>,
    /// The lines the block holds, which are the ones to add by hand after a `conflict`.
    pub lines: Vec<String>,
}

impl HooksReport {
    /// The hooks of the block that are not set outside it, whose lines a `conflict` leaves for the person to add as
    /// they are.
    #[must_use]
    pub fn unset(&self) -> Vec<&str> {
        self.lines
            .iter()
            .filter_map(|line| line.split_once(" = ").map(|(hook, _)| hook))
            .filter(|hook| !self.set.iter().any(|set| set == hook))
            .collect()
    }
}

/// What the Daemon answers a `Doctor` request with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct DaemonChecks {
    /// The version of the procly executable the Daemon runs, which the client compares with its own. Not `version`,
    /// which the envelope of every message already carries.
    pub procly_version: String,
    /// The checks only the Daemon can answer.
    pub checks: Vec<Check>,
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
        assert_eq!(line, "{\"version\":22,\"request\":\"wait\",\"job\":\"3f9a1c2e\"}\n");
        assert!(matches!(decode::<Request>(&line), Ok(Request::Wait { job }) if job == "3f9a1c2e"));
    }

    #[test]
    fn a_restart_names_the_service_and_the_directory_it_was_asked_from() {
        let line = encode(Request::Restart {
            service: "web".to_owned(),
            cwd: PathBuf::from("/src/app"),
        })
        .unwrap();
        assert_eq!(
            line,
            "{\"version\":22,\"request\":\"restart\",\"service\":\"web\",\"cwd\":\"/src/app\"}\n"
        );
        assert!(matches!(
            decode::<Request>(&line),
            Ok(Request::Restart { service, cwd }) if service == "web" && cwd.as_os_str() == "/src/app"
        ));
    }

    #[test]
    fn a_worktree_release_asks_for_a_dry_run_only_when_it_says_so() {
        let line = encode(Request::WorktreeRelease {
            path: PathBuf::from("/src/app"),
            dry_run: true,
        })
        .unwrap();
        assert_eq!(
            line,
            "{\"version\":22,\"request\":\"worktree_release\",\"path\":\"/src/app\",\"dry_run\":true}\n"
        );
        assert!(matches!(
            decode::<Request>(&line),
            Ok(Request::WorktreeRelease { dry_run: true, .. })
        ));

        // A request from a client that never heard of the flag is a real release.
        let absent = "{\"version\":22,\"request\":\"worktree_release\",\"path\":\"/src/app\"}\n";
        assert!(matches!(
            decode::<Request>(absent),
            Ok(Request::WorktreeRelease { dry_run: false, .. })
        ));
    }

    #[test]
    fn an_orphan_carries_its_kind_beside_its_worktree_and_round_trips() {
        let orphan = lease_orphan(false, None);
        let line = encode(Response::Gc(GcReport {
            applied: false,
            orphans: vec![orphan.clone()],
            skipped: Vec::new(),
            warnings: Vec::new(),
        }))
        .unwrap();
        assert!(line.contains(
            r#"{"kind":"lease","service":"web","port_name":"http","port":24000,"repository":"/src/app/.git","#
        ));
        assert!(matches!(decode::<Response>(&line), Ok(Response::Gc(report)) if report.orphans == [orphan]));
    }

    fn lease_orphan(deleted: bool, error: Option<&str>) -> Orphan {
        Orphan {
            id: OrphanId {
                kind: OrphanKind::Lease {
                    service: "web".to_owned(),
                    port_name: "http".to_owned(),
                    port: 24000,
                },
                repository: PathBuf::from("/src/app/.git"),
                worktree: PathBuf::from("/src/app-a"),
            },
            deleted,
            error: error.map(str::to_owned),
        }
    }

    #[test]
    fn an_orphan_serializes_exactly_as_it_did_before_its_identity_was_named() {
        let line = serde_json::to_string(&lease_orphan(false, None)).unwrap();

        assert_eq!(
            line,
            r#"{"kind":"lease","service":"web","port_name":"http","port":24000,"repository":"/src/app/.git","worktree":"/src/app-a","deleted":false,"error":null}"#
        );
    }

    #[test]
    fn the_orphans_of_a_dry_run_sent_back_as_returned_deserialize_as_only() {
        let report = GcReport {
            applied: false,
            orphans: vec![lease_orphan(true, Some("kept"))],
            ..GcReport::default()
        };
        let answer: serde_json::Value = serde_json::from_str(&serde_json::to_string(&report).unwrap()).unwrap();
        let request = format!(
            r#"{{"version":{PROTOCOL_VERSION},"request":"gc","cwd":"/src/app","apply":true,"only":{}}}"#,
            answer["orphans"]
        );

        let decoded = decode::<Request>(&request);

        let Ok(Request::Gc {
            apply: true,
            only: Some(only),
            ..
        }) = decoded
        else {
            panic!("not a Gc request limited to Orphans: {decoded:?}");
        };
        assert_eq!(only, [lease_orphan(false, None).id]);
    }

    #[test]
    fn a_gc_request_without_only_lists_or_deletes_every_orphan() {
        let absent = format!(r#"{{"version":{PROTOCOL_VERSION},"request":"gc","cwd":"/src/app","apply":true}}"#);

        assert!(matches!(decode::<Request>(&absent), Ok(Request::Gc { only: None, .. })));
        let line = encode(Request::Gc {
            cwd: PathBuf::from("/src/app"),
            apply: true,
            only: None,
        })
        .unwrap();
        assert!(!line.contains("only"), "{line}");
    }

    fn worktree_status(branch: Option<&str>, listed: bool) -> WorktreeStatus {
        WorktreeStatus {
            worktree: Worktree {
                path: PathBuf::from("/src/app"),
                slug: "app".to_owned(),
                main: true,
            },
            branch: branch.map(str::to_owned),
            listed,
            services: vec![ServiceStatus {
                service: "web".to_owned(),
                port: Some(24000),
                reattached: false,
                started_at: 1_793_900_000_123,
                restartable: true,
                state: ServiceState::Healthy,
            }],
            jobs: Vec::new(),
            foreign: Vec::new(),
            containers: Vec::new(),
        }
    }

    fn status_line(worktree: WorktreeStatus) -> String {
        encode(Response::Status(StatusReport {
            repositories: vec![RepositoryStatus {
                git_common_dir: PathBuf::from("/src/app/.git"),
                worktrees: vec![worktree],
            }],
            jobs_outside_repositories: Vec::new(),
        }))
        .unwrap()
    }

    #[test]
    fn the_branch_the_listed_flag_and_the_start_time_round_trip_through_a_line() {
        let line = status_line(worktree_status(Some("feature/login"), true));
        assert!(line.contains(r#""branch":"feature/login","listed":true,"services":[{"service":"web","port":24000,"reattached":false,"started_at":1793900000123,"restartable":true,"state":"healthy"}]"#));
        let Ok(Response::Status(report)) = decode::<Response>(&line) else {
            panic!("the line decodes to a status report: {line}");
        };
        let status = &report.repositories[0].worktrees[0];
        assert_eq!(status.branch.as_deref(), Some("feature/login"));
        assert!(status.listed);
        assert_eq!(status.services[0].started_at, 1_793_900_000_123);
    }

    #[test]
    fn a_worktree_without_a_branch_leaves_the_key_out_and_a_removed_one_says_it_is_not_listed() {
        let line = status_line(worktree_status(None, false));
        assert!(!line.contains("branch"));
        assert!(line.contains(r#""listed":false"#));
        let Ok(Response::Status(report)) = decode::<Response>(&line) else {
            panic!("the line decodes to a status report: {line}");
        };
        let status = &report.repositories[0].worktrees[0];
        assert_eq!(status.branch, None);
        assert!(!status.listed);
    }

    #[test]
    fn a_logs_request_without_follow_asks_for_the_backlog_only() {
        let line = r#"{"version":22,"request":"logs","name":"api","cwd":"/src/app","env":[]}"#;
        assert!(matches!(
            decode::<Request>(line),
            Ok(Request::Logs { follow: false, .. })
        ));
    }

    #[test]
    fn the_responses_of_a_follow_round_trip_through_a_line() {
        let output = encode(Response::Output {
            text: "ready\n".to_owned(),
        })
        .unwrap();
        assert_eq!(
            output,
            "{\"version\":22,\"response\":\"output\",\"text\":\"ready\\n\"}\n"
        );
        assert!(matches!(decode::<Response>(&output), Ok(Response::Output { text }) if text == "ready\n"));
        let dropped = encode(Response::OutputDropped { bytes: 4096 }).unwrap();
        assert_eq!(
            dropped,
            "{\"version\":22,\"response\":\"output_dropped\",\"bytes\":4096}\n"
        );
        assert!(matches!(
            decode::<Response>(&dropped),
            Ok(Response::OutputDropped { bytes: 4096 })
        ));
        let ended = encode(Response::LogsEnded).unwrap();
        assert_eq!(ended, "{\"version\":22,\"response\":\"logs_ended\"}\n");
        assert!(matches!(decode::<Response>(&ended), Ok(Response::LogsEnded)));
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
