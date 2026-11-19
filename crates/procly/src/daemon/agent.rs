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
use crate::config;
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
        Request::Guard { cwd } => guard(&registry, &cwd),
        other => unreachable!("only agent requests are handed to the agent module, not {other:?}"),
    };
    blocking("the agent request could not be answered", move || Ok(work()?))
        .await
        .unwrap_or_else(|message| Response::Error { message })
}

/// The Rules block, one line per element. When the Repository is Off it says so, and tells agents to leave Procly be.
/// Otherwise it names the declared Services with `services`, the line [`services_line`] gives, when there is one.
pub(super) fn rules(off: bool, services: Option<&str>) -> Vec<&str> {
    if off {
        vec![
            "## Procly (off)",
            "",
            "Procly is off for this repository. Start dev servers and containers with their usual commands, and do not run `procly up`.",
            "",
            "The developer turns it back on with `procly on`.",
        ]
    } else {
        let mut lines = vec![
            "## Procly",
            "",
            "Dev servers, queue workers and containers of this repository run under Procly, so that parallel worktrees never start the same process twice or fight over a port.",
            "",
        ];
        lines.extend(services);
        lines.extend([
            "- Start a service with `procly up <service>`. It answers `started` or `reused`, with each of its ports and, when known, its URL.",
            "- Stop one with `procly down <service>`, or all of them with `procly down`. See what runs with `procly status`.",
            "- Run a one-off command with `procly run \"<command>\"`, then read its result with `procly wait <job>`.",
            "- Never start a dev server or `docker compose up` yourself. A hook blocks the known commands and names the Procly command to run instead.",
        ]);
        lines
    }
}

/// The line of the Rules block naming the Services the versioned `procly.toml` of `worktree` declares, in the order
/// of the file, the Groups last with the Services each one covers there. The local file is personal and excluded from
/// git, so what it declares never ends in an instruction file that is committed. `None` when the versioned file
/// declares no Service, or when the configuration cannot be read: the Rules block is still written, without the line.
/// Blocking, it reads the configuration.
pub(super) fn services_line(worktree: &Path) -> Option<String> {
    // A configuration that is not valid names nothing, as `procly up` would refuse it.
    read_config(worktree).ok()?;
    let text = std::fs::read_to_string(worktree.join(config::VERSIONED_FILE)).ok()?;
    let declared = versioned_services(&text);
    let borrowed: Vec<(&str, Option<&[String]>)> = declared
        .iter()
        .map(|(name, covers)| (name.as_str(), covers.as_deref()))
        .collect();
    naming(&borrowed)
}

/// The line naming the `declared` Services, in their order, each with the Services it covers when it is a Group.
fn naming(declared: &[(&str, Option<&[String]>)]) -> Option<String> {
    let quoted = |name: &str| format!("`{name}`");
    let mut items: Vec<String> = declared
        .iter()
        .filter(|(_, covers)| covers.is_none())
        .map(|(name, _)| quoted(name))
        .collect();
    items.extend(declared.iter().filter_map(|(name, covers)| {
        let covered: Vec<String> = (*covers)?.iter().map(|name| quoted(name)).collect();
        Some(format!(
            "the Group {} covering {}",
            quoted(name),
            listed(&covered, false)
        ))
    }));
    (!items.is_empty()).then(|| format!("- The services of this repository: {}.", listed(&items, true)))
}

/// `items` as a list in a sentence, the last one after `and`, with a comma before it in a list of 3 or more when
/// `serial`.
fn listed(items: &[String], serial: bool) -> String {
    match items {
        [] => String::new(),
        [only] => only.clone(),
        [first, second] => format!("{first} and {second}"),
        [rest @ .., last] => {
            let comma = if serial { "," } else { "" };
            format!("{}{comma} and {last}", rest.join(", "))
        }
    }
}

/// The Services `text`, a versioned `procly.toml`, declares, in the order of the file, each with the Services it covers
/// when it is a Group. A file that cannot be parsed declares none.
fn versioned_services(text: &str) -> Vec<(String, Option<Vec<String>>)> {
    let Ok(document) = toml::de::DeTable::parse(text) else {
        return Vec::new();
    };
    let Some(services) = document
        .get_ref()
        .get("services")
        .and_then(|services| services.get_ref().as_table())
    else {
        return Vec::new();
    };
    let mut declared: Vec<_> = services
        .iter()
        .map(|(name, service)| {
            let covers = service
                .get_ref()
                .as_table()
                .and_then(|service| service.get("covers"))
                .and_then(|covers| covers.get_ref().as_array())
                .map(|covers| {
                    covers
                        .iter()
                        .filter_map(|covered| covered.get_ref().as_str().map(str::to_owned))
                        .collect()
                });
            (name.span().start, name.get_ref().to_string(), covers)
        })
        .collect();
    declared.sort_by_key(|(start, _, _)| *start);
    declared.into_iter().map(|(_, name, covers)| (name, covers)).collect()
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
    let services = services_line(worktree);
    let rules = registry
        .managed
        .write_apart(&instructions, RULES, &rules(off, services.as_deref()))?;
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
        // The deepest Worktree holding the file, since a Worktree may sit inside another one.
        let ours = worktrees
            .iter()
            .filter(|worktree| file.starts_with(&worktree.path))
            .max_by_key(|worktree| worktree.path.components().count());
        if let Some(worktree) = ours
            && ManagedBlocks::holds(&file, RULES)?
        {
            // The Services are read again, from the Worktree of the file, as `agent install` would.
            let services = services_line(&worktree.path);
            let outcome = registry
                .managed
                .write_apart(&file, RULES, &rules(off, services.as_deref()))?;
            reports.push(BlockReport {
                file,
                outcome: outcome.into(),
            });
        }
    }
    Ok(SwitchReport { off, rules: reports })
}

/// Whether the Repository holding `cwd` is Off, where `cwd` is relative to the root of its Worktree (`None` when it cannot be placed), and the Services the
/// Worktree declares with the compose file it declares, which the Guard matches the command it blocks against and
/// names in its answer. A configuration that cannot be read declares none: the Guard still answers.
pub(super) fn guard(registry: &Registry, cwd: &Path) -> Result<Response, AgentError> {
    let topology = topology_in(cwd)?;
    let off = registry.offs.is_off(&topology.git_common_dir)?;
    let directory = topology::canonicalize(cwd)
        .ok()
        .and_then(|cwd| cwd.strip_prefix(&topology.worktree.path).map(Path::to_owned).ok());
    let (services, compose_file) = read_config(&topology.worktree.path)
        .map(|config| {
            let services = config
                .services
                .into_iter()
                .map(|(name, declared)| GuardService {
                    name,
                    run: declared.run,
                    compose: declared.compose,
                })
                .collect();
            (services, config.settings.compose_file)
        })
        .unwrap_or_default();
    Ok(Response::Guard {
        off,
        directory,
        root: topology.worktree.path,
        services,
        compose_file,
    })
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
pub(super) fn topology_in(cwd: &Path) -> Result<Topology, AgentError> {
    let cwd = topology::canonicalize(cwd)?;
    topology::resolve(&cwd)?.ok_or(AgentError::Topology(TopologyError::NotARepository { dir: cwd }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn one_service_is_named_alone() {
        assert_eq!(
            naming(&[("web", None)]).as_deref(),
            Some("- The services of this repository: `web`.")
        );
    }

    #[test]
    fn two_services_are_joined_by_and() {
        assert_eq!(
            naming(&[("web", None), ("api", None)]).as_deref(),
            Some("- The services of this repository: `web` and `api`.")
        );
    }

    #[test]
    fn the_groups_come_last_with_the_services_they_cover() {
        let (front, all) = (names(&["web", "api"]), names(&["web", "api", "worker"]));
        assert_eq!(
            naming(&[
                ("full", Some(&all)),
                ("web", None),
                ("api", None),
                ("worker", None),
                ("dev", Some(&front)),
            ])
            .as_deref(),
            Some(
                "- The services of this repository: `web`, `api`, `worker`, the Group `full` covering `web`, `api` \
                 and `worker`, and the Group `dev` covering `web` and `api`."
            )
        );
    }

    #[test]
    fn no_declared_service_gives_no_line() {
        assert_eq!(naming(&[]), None);
    }

    #[test]
    fn the_services_are_placed_in_the_order_the_versioned_file_declares_them() {
        let declared = versioned_services(
            "[services.web]\nrun = 'y'\n[services.dev]\ncovers = ['web', 'api']\n[services.api]\nrun = 'x'\n\
             [services.web.health]\npath = '/'\n[settings]\nservices.zeta.run = 'z'\n",
        );

        assert_eq!(
            declared,
            vec![
                ("web".to_owned(), None),
                ("dev".to_owned(), Some(names(&["web", "api"]))),
                ("api".to_owned(), None),
            ]
        );
    }

    #[test]
    fn a_versioned_file_that_cannot_be_parsed_declares_no_service() {
        assert_eq!(versioned_services("[services.web"), Vec::new());
    }
}
