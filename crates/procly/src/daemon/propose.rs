//! `procly init`: writes the `procly.toml` proposed for a Worktree, unless it has one already.

use std::fs::OpenOptions;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use super::topology_of;
use crate::config::VERSIONED_FILE;
use crate::proposal::{self, ProposalError};
use crate::protocol::{ProposalReport, Response};

#[derive(Debug, thiserror::Error)]
enum ProposeError {
    #[error(transparent)]
    Proposal(#[from] ProposalError),
    #[error("cannot write {path}")]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Answers an `Init` request. Resolving the Topology runs git, and reading the Worktree and writing a file are
/// blocking, so the work runs on a thread that may block.
pub(super) async fn run(cwd: PathBuf) -> Response {
    match tokio::task::spawn_blocking(move || propose(&cwd)).await {
        Ok(Ok(report)) => Response::Init(report),
        Ok(Err(error)) => Response::Error {
            message: format!("{error:#}"),
        },
        Err(error) => Response::Error {
            message: format!("the proposal could not be made: {error}"),
        },
    }
}

fn propose(cwd: &Path) -> anyhow::Result<ProposalReport> {
    let worktree = topology_of(cwd)?.worktree.path;
    let file = worktree.join(VERSIONED_FILE);
    let toml = proposal::propose(&worktree).map_err(ProposeError::from)?;
    let written = write_new(&file, &toml)?;
    Ok(ProposalReport { file, toml, written })
}

/// Writes `text` to `file` unless it exists, and tells whether it did. The file is created exclusively, so that an
/// existing one is never overwritten, not even by a `procly.toml` that appeared since the proposal was made.
fn write_new(file: &Path, text: &str) -> Result<bool, ProposeError> {
    let write = |source| ProposeError::Write {
        path: file.to_owned(),
        source,
    };
    let mut created = match OpenOptions::new().write(true).create_new(true).open(file) {
        Ok(created) => created,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Ok(false),
        Err(error) => return Err(write(error)),
    };
    if let Err(error) = created.write_all(text.as_bytes()) {
        // The file is the one just created, so a half-written one is removed rather than left to be read as a config.
        let _ = std::fs::remove_file(file);
        return Err(write(error));
    }
    Ok(true)
}
