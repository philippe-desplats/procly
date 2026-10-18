//! The versioned request and response protocol spoken over the Daemon socket.
//!
//! One connection carries one request and one response, each a single line of JSON.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::topology::Worktree;

/// Bumped on any incompatible change, the Daemon refuses a request carrying another version.
pub(crate) const PROTOCOL_VERSION: u32 = 6;

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
    Wait {
        job: String,
    },
    Logs {
        job: String,
    },
    /// Starts a declared Service of the Worktree holding `cwd`, or finds it running, and answers once it is healthy.
    Up {
        service: String,
        cwd: PathBuf,
        /// The environment of the requesting command, as for `Run`.
        env: Vec<(String, String)>,
        /// Writes the exclusion of the generated files to the `.gitignore` of the Worktree and not to the shared git
        /// exclude file, unless a block of Procly already sits in the `.gitignore`.
        gitignore: bool,
    },
    /// The variables Procly injects into the Service `service` of the Worktree holding `cwd`, without starting it.
    Env {
        service: String,
        cwd: PathBuf,
    },
    /// Stops a Service of the Worktree holding `cwd`, or every Service of it without `service`, and answers once
    /// their trees are gone.
    Down {
        service: Option<String>,
        cwd: PathBuf,
    },
    /// The Topology and the Jobs of the Worktree holding `cwd`, or of every Worktree with `all`.
    Status {
        cwd: PathBuf,
        all: bool,
    },
    /// Prepares the Worktree holding `path`: copies the listed files from the Main worktree, then runs the setup
    /// commands it has not run yet, and answers once they have ended.
    WorktreeInit {
        path: PathBuf,
        /// The environment of the requesting command, handed to the setup commands as for `Run`.
        env: Vec<(String, String)>,
    },
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
    },
    /// The Service ended before it became healthy.
    Unhealthy {
        service: String,
        outcome: StartOutcome,
        end: ServiceEnd,
        /// The recent output of the Service, to tell why.
        output: String,
    },
    /// The variables Procly injects into the Service, by name.
    Env {
        variables: Vec<(String, String)>,
    },
    /// The Services `down` stopped, by name.
    Down {
        stopped: Vec<String>,
    },
    /// The Worktree is prepared.
    WorktreeInit(InitReport),
    /// A setup command failed, and the ones after it were not run.
    SetupFailed {
        report: InitReport,
        command: String,
        end: JobEnd,
        /// The recent output of the command, to tell why.
        output: String,
    },
    Error {
        message: String,
    },
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
    /// The process group of the Service listened on `actual` and not on its Lease `leased`, and its whole process
    /// tree was stopped.
    PortMismatch {
        leased: u16,
        actual: u16,
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
    pub(crate) jobs: Vec<JobStatus>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_message_round_trips_through_a_line() {
        let line = encode(Request::Wait {
            job: "3f9a1c2e".to_owned(),
        })
        .unwrap();
        assert_eq!(line, "{\"version\":6,\"request\":\"wait\",\"job\":\"3f9a1c2e\"}\n");
        assert!(matches!(decode::<Request>(&line), Ok(Request::Wait { job }) if job == "3f9a1c2e"));
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
