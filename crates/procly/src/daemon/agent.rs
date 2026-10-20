//! Steering agents to Procly: the Rules block, the Guard and the switch that makes both stand aside (ADR 0008).
//!
//! Procly writes into an instruction file or a settings file only when `agent install` asks it to, and only inside
//! the Worktree. Nothing here ever touches a global file.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::hook::{self, HookError};
use super::managed::{ManagedBlocks, ManagedError, Outcome};
use super::store::StoreError;
use super::{Registry, blocking, read_config};
use crate::protocol::{BlockOutcome, BlockReport, GuardService, InstallReport, Request, Response, SwitchReport};
use crate::topology::{self, Topology, TopologyError};

/// The name of the Managed block holding the Rules block.
pub(super) const RULES: &str = "rules";
/// The Claude Code settings of a Worktree, relative to its root.
pub(super) const SETTINGS: &str = ".claude/settings.json";
/// The instruction files Procly looks for, in order, when no file is named.
pub(super) const INSTRUCTION_FILES: [&str; 2] = ["AGENTS.md", "CLAUDE.md"];

#[derive(Debug, thiserror::Error)]
pub(super) enum AgentError {
    #[error(transparent)]
    Topology(#[from] TopologyError),
    #[error(transparent)]
    Managed(#[from] ManagedError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Hook(#[from] HookError),
    #[error("{file} is outside the Worktree {worktree}, and Procly writes no instruction file outside it")]
    OutsideWorktree { file: PathBuf, worktree: PathBuf },
    #[error("cannot resolve {path}")]
    Resolve {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

impl From<Outcome> for BlockOutcome {
    fn from(outcome: Outcome) -> Self {
        match outcome {
            Outcome::Written => Self::Written,
            Outcome::Unchanged => Self::Unchanged,
            Outcome::Edited => Self::Edited,
        }
    }
}

/// Answers an `AgentInstall`, `Switch` or `Guard` request. Resolving the Topology runs git and writing a file is
/// blocking, so the work runs on a thread that may block.
pub(super) async fn answer(registry: &Arc<Registry>, request: Request) -> Response {
    let registry = Arc::clone(registry);
    let work = move || match request {
        Request::AgentInstall { cwd, file } => install(&registry, &cwd, file.as_deref()).map(Response::AgentInstall),
        Request::Switch { cwd, off } => switch(&registry, &cwd, off).map(Response::Switched),
        Request::Guard { cwd } => guard(&registry, &cwd).map(|(off, directory, root, services)| Response::Guard {
            off,
            directory,
            root,
            services,
        }),
        other => unreachable!("only agent requests are handed to the agent module, not {other:?}"),
    };
    blocking("the agent request could not be answered", move || Ok(work()?))
        .await
        .unwrap_or_else(|message| Response::Error { message })
}

/// The Rules block, one line per element. When the Repository is Off it says so, and tells agents to leave Procly be.
pub(super) fn rules(off: bool) -> &'static [&'static str] {
    if off {
        &[
            "## Procly (off)",
            "",
            "Procly is off for this repository. Start dev servers and containers with their usual commands, and do not run `procly up`.",
            "",
            "The developer turns it back on with `procly on`.",
        ]
    } else {
        &[
            "## Procly",
            "",
            "Dev servers, queue workers and containers of this repository run under Procly, so that parallel worktrees never start the same process twice or fight over a port.",
            "",
            "- Start a service with `procly up <service>`. It answers `started` or `reused`, with each of its ports and, when known, its URL.",
            "- Stop one with `procly down <service>`, or all of them with `procly down`. See what runs with `procly status`.",
            "- Run a one-off command with `procly run \"<command>\"`, then read its result with `procly wait <job>`.",
            "- Never start a dev server or `docker compose up` yourself. A hook blocks the known commands and names the Procly command to run instead.",
        ]
    }
}

/// Writes the Rules block into the instruction file and the Guard into the Claude Code settings of the Worktree
/// holding `cwd`. Without `file`, the instruction file is the `AGENTS.md` of the Worktree, or its `CLAUDE.md` when
/// that is the only one, and `AGENTS.md` is created when there is neither.
pub(super) fn install(registry: &Registry, cwd: &Path, file: Option<&Path>) -> Result<InstallReport, AgentError> {
    let _installing = registry
        .agent
        .lock()
        .expect("the agent mutex is never poisoned, no code panics while holding it");
    let topology = topology_in(cwd)?;
    let worktree = &topology.worktree.path;
    let instructions = contained(worktree, &instruction_file(worktree, file))?;
    let off = registry.offs.is_off(&topology.git_common_dir)?;

    // The settings first, since a file that is not JSON is the likelier failure and nothing is written then.
    let settings = contained(worktree, &worktree.join(SETTINGS))?;
    let guard = hook::install(&registry.managed, &settings)?;
    let rules = registry.managed.write(&instructions, RULES, rules(off))?;
    Ok(InstallReport {
        rules: BlockReport {
            file: instructions,
            outcome: rules.into(),
        },
        guard: BlockReport {
            file: settings,
            outcome: guard.into(),
        },
        off,
    })
}

/// Switches the Repository holding `cwd` Off or back On, and rewrites the Rules blocks Procly wrote in its Worktrees
/// to match. A Rules block the user edited is left as it is, and one whose file lost it is not written back.
pub(super) fn switch(registry: &Registry, cwd: &Path, off: bool) -> Result<SwitchReport, AgentError> {
    let _switching = registry
        .agent
        .lock()
        .expect("the agent mutex is never poisoned, no code panics while holding it");
    let topology = topology_in(cwd)?;
    registry.offs.set(&topology.git_common_dir, off)?;
    let worktrees = topology::worktrees(&topology.git_common_dir)?;
    let mut reports = Vec::new();
    for file in registry.managed.files_holding(RULES)? {
        let ours = worktrees.iter().any(|worktree| file.starts_with(&worktree.path));
        if ours && ManagedBlocks::holds(&file, RULES)? {
            let outcome = registry.managed.write(&file, RULES, rules(off))?;
            reports.push(BlockReport {
                file,
                outcome: outcome.into(),
            });
        }
    }
    Ok(SwitchReport { off, rules: reports })
}

/// Whether the Repository holding `cwd` is Off, where `cwd` is relative to the root of its Worktree (`None` when it cannot be placed), and the Services the
/// Worktree declares, which the Guard matches the command it blocks against and names in its answer. A configuration
/// that cannot be read declares none: the Guard still answers.
pub(super) fn guard(
    registry: &Registry,
    cwd: &Path,
) -> Result<(bool, Option<PathBuf>, PathBuf, Vec<GuardService>), AgentError> {
    let topology = topology_in(cwd)?;
    let off = registry.offs.is_off(&topology.git_common_dir)?;
    let directory = topology::canonicalize(cwd)
        .ok()
        .and_then(|cwd| cwd.strip_prefix(&topology.worktree.path).map(Path::to_owned).ok());
    let services = read_config(&topology.worktree.path)
        .map(|config| {
            config
                .services
                .into_iter()
                .map(|(name, declared)| GuardService {
                    name,
                    run: declared.run,
                    compose: declared.compose,
                })
                .collect()
        })
        .unwrap_or_default();
    Ok((off, directory, topology.worktree.path, services))
}

/// The instruction file to write: the one asked for, or the default one.
fn instruction_file(worktree: &Path, requested: Option<&Path>) -> PathBuf {
    requested.map_or_else(
        || {
            let existing = INSTRUCTION_FILES.iter().find(|name| worktree.join(name).is_file());
            worktree.join(existing.unwrap_or(&INSTRUCTION_FILES[0]))
        },
        Path::to_owned,
    )
}

/// `requested` with its symlinks resolved, which must sit inside the Worktree.
pub(super) fn contained(worktree: &Path, requested: &Path) -> Result<PathBuf, AgentError> {
    let outside = || AgentError::OutsideWorktree {
        file: requested.to_owned(),
        worktree: worktree.to_owned(),
    };
    // The part of the path that does not exist yet is added back to the part that does. A `..` in it is refused, since
    // it could climb out of the Worktree after the check.
    let mut existing = requested;
    let mut missing = Vec::new();
    while !existing.exists() {
        missing.push(existing.file_name().ok_or_else(outside)?);
        existing = existing.parent().ok_or_else(outside)?;
    }
    // Through symlinks, so that a link pointing out of the Worktree is refused, and one pointing inside is written
    // through instead of being replaced by a regular file.
    let mut resolved = existing.canonicalize().map_err(|source| AgentError::Resolve {
        path: existing.to_owned(),
        source,
    })?;
    resolved.extend(missing.iter().rev());
    if resolved.starts_with(worktree) && !resolved.is_dir() {
        Ok(resolved)
    } else {
        Err(outside())
    }
}

/// The Repository and the Worktree holding `cwd`, which must be in a git Repository.
fn topology_in(cwd: &Path) -> Result<Topology, AgentError> {
    let cwd = topology::canonicalize(cwd)?;
    topology::resolve(&cwd)?.ok_or(AgentError::Topology(TopologyError::NotARepository { dir: cwd }))
}
