//! The Services proposed from the `dev` scripts of the `package.json` files of a Worktree.

use std::fs;
use std::io;
use std::path::Path;

use serde_json::Value;

use super::{Proposal, ProposalError, ServiceEntry, service_name};

/// The JavaScript package manager a Worktree installs with, which runs its scripts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Manager {
    Npm,
    Pnpm,
    Yarn,
    Bun,
}

impl Manager {
    /// Read from the lockfile the Worktree holds, `npm` when it holds none.
    fn of(worktree: &Path) -> Self {
        [
            ("pnpm-lock.yaml", Self::Pnpm),
            ("bun.lock", Self::Bun),
            ("bun.lockb", Self::Bun),
            ("yarn.lock", Self::Yarn),
        ]
        .into_iter()
        .find(|(lockfile, _)| worktree.join(lockfile).exists())
        .map_or(Self::Npm, |(_, manager)| manager)
    }

    /// The command that runs the script `script`.
    fn run(self, script: &str) -> String {
        let program = match self {
            Self::Npm => "npm",
            Self::Pnpm => "pnpm",
            Self::Yarn => "yarn",
            Self::Bun => "bun",
        };
        format!("{program} run {script}")
    }
}

/// Reads the `package.json` in `dir`, `None` when there is none.
fn read_package(dir: &Path) -> Result<Option<Value>, ProposalError> {
    let path = dir.join("package.json");
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(ProposalError::Read { path, source }),
    };
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|source| ProposalError::Json { path, source })
}

/// Adds the Services the `dev` scripts of the Worktree rooted at `worktree` call for.
pub(super) fn propose(worktree: &Path, proposal: &mut Proposal) -> Result<(), ProposalError> {
    let Some(root) = read_package(worktree)? else {
        return Ok(());
    };
    if root.pointer("/scripts/dev").and_then(Value::as_str).is_some() {
        let name = root.get("name").and_then(Value::as_str).unwrap_or_default();
        proposal.add(
            &service_name(name, "app"),
            ServiceEntry {
                run: Some(Manager::of(worktree).run("dev")),
                ..ServiceEntry::default()
            },
        );
    }
    Ok(())
}
