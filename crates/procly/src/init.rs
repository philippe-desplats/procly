//! `procly init`: writes the `procly.toml` proposed for a Worktree, unless it has one already. The proposal reads the
//! Worktree only, so init runs in the CLI and never starts the Daemon.

use std::fs::OpenOptions;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::config::VERSIONED_FILE;
use crate::proposal::{self, ProposalError};
use crate::topology::{self, Topology, TopologyError};

#[derive(Debug, thiserror::Error)]
pub(crate) enum InitError {
    #[error(transparent)]
    Topology(#[from] TopologyError),
    #[error("{} is not in a git repository, and Services are declared per Repository", .0.display())]
    NotInRepository(PathBuf),
    #[error(transparent)]
    Proposal(#[from] ProposalError),
    #[error("cannot write {path}")]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// What `init` proposed.
#[derive(Debug, Serialize)]
pub(crate) struct ProposalReport {
    /// The `procly.toml` of the Worktree, written there when `written`.
    pub(crate) file: PathBuf,
    /// The proposal, the content of `file` when `written`.
    pub(crate) toml: String,
    /// Whether the Worktree had no `procly.toml`. One that exists is never overwritten.
    pub(crate) written: bool,
    /// The choices the proposal made that deserve a second look. They go to stderr, the document does not carry them.
    #[serde(skip)]
    pub(crate) notes: Vec<String>,
}

/// Proposes a `procly.toml` for the Worktree holding `cwd`, and writes it at the root of that Worktree unless it has
/// one.
pub(crate) fn propose(cwd: &Path) -> Result<ProposalReport, InitError> {
    let cwd = topology::canonicalize(cwd)?;
    let Some(topology) = topology::resolve(&cwd)? else {
        return Err(InitError::NotInRepository(cwd));
    };
    let repository = repository_name(&topology)?;
    let worktree = topology.worktree.path;
    let file = worktree.join(VERSIONED_FILE);
    let proposed = proposal::propose(&worktree, &repository)?;
    let written = write_new(&file, &proposed.toml)?;
    Ok(ProposalReport {
        file,
        toml: proposed.toml,
        written,
        notes: proposed.notes,
    })
}

/// The name of the Repository: the directory of its Main worktree, the same one from every Worktree. A Repository with
/// no Main worktree is named after the Worktree.
fn repository_name(topology: &Topology) -> Result<String, TopologyError> {
    let root = match topology.main() {
        Ok(main) => main.worktree.path,
        Err(TopologyError::NoMainWorktree { .. }) => topology.worktree.path.clone(),
        Err(error) => return Err(error),
    };
    Ok(root.file_name().unwrap_or_default().to_string_lossy().into_owned())
}

/// Writes `text` to `file` unless it exists, and tells whether it did. The file is created exclusively, so that an
/// existing one is never overwritten, not even by a `procly.toml` that appeared since the proposal was made.
fn write_new(file: &Path, text: &str) -> Result<bool, InitError> {
    let write = |source| InitError::Write {
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
