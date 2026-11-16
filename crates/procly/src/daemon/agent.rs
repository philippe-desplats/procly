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
use crate::config::{self, ServiceConfig};
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

/// The line of the Rules block naming the Services the configuration of `worktree` declares, in the order of its
/// files, the Groups last with the Services each one covers. `None` when it declares no Service, or when it cannot be
/// read: the Rules block is still written, without the line. Blocking, it reads the configuration.
pub(super) fn services_line(worktree: &Path) -> Option<String> {
    let config = read_config(worktree).ok()?;
    let order = declaration_order(worktree);
    let position = |name: &str| order.iter().position(|declared| declared == name).unwrap_or(usize::MAX);
    let mut declared: Vec<(&str, Option<&[String]>)> = config
        .services
        .iter()
        .map(|(name, service): (&String, &ServiceConfig)| (name.as_str(), service.covers.as_deref()))
        .collect();
    // Stable, so that a Service no file places, which cannot happen once the configuration is read, stays sorted.
    declared.sort_by_key(|(name, _)| position(name));
    naming(&declared)
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

/// The names of the Services declared in the configuration files of `worktree`, in the order they first appear, the
/// versioned file first. A file that cannot be read or parsed places none.
fn declaration_order(worktree: &Path) -> Vec<String> {
    let mut order: Vec<String> = Vec::new();
    for file in [config::VERSIONED_FILE, config::LOCAL_FILE] {
        let Ok(text) = std::fs::read_to_string(worktree.join(file)) else {
            continue;
        };
        let Ok(document) = toml::de::DeTable::parse(&text) else {
            continue;
        };
        let Some(toml::de::DeValue::Table(services)) = document.get_ref().get("services").map(|value| value.get_ref())
        else {
            continue;
        };
        let mut names: Vec<_> = services
            .keys()
            .map(|name| (name.span().start, name.get_ref().to_string()))
            .collect();
        names.sort();
        for (_, name) in names {
            if !order.contains(&name) {
                order.push(name);
            }
        }
    }
    order
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
        let ours = worktrees.iter().find(|worktree| file.starts_with(&worktree.path));
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
    fn the_services_are_placed_in_the_order_the_files_declare_them() {
        let worktree = tempfile::tempdir().expect("a temporary directory");
        std::fs::write(
            worktree.path().join(config::VERSIONED_FILE),
            "[services.web]\nrun = 'y'\n[services.api]\nrun = 'x'\n[services.web.health]\npath = '/'\n\
             [settings]\nservices.zeta.run = 'z'\n",
        )
        .expect("write the configuration");
        std::fs::write(
            worktree.path().join(config::LOCAL_FILE),
            "services.local.run = 'l'\nservices.api.run = 'x2'\n",
        )
        .expect("write the local configuration");

        assert_eq!(declaration_order(worktree.path()), names(&["web", "api", "local"]));
    }
}
