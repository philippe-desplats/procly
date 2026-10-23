//! Worktrunk hooks: the Managed block of `.config/wt.toml` that makes Worktrunk run `procly worktree init` when it
//! creates a Worktree and `procly worktree release` before it removes one (ADR 0008, ADR 0011).
//!
//! Procly writes the block only when `worktree hooks` asks it to, and never next to a hook the developer set: Worktrunk
//! keeps one command per hook, so a second `pre-start` would be an invalid file or a hook that silently wins.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::agent::{AgentError, contained, topology_in};
use super::hook::Standing;
use super::managed::{self, Content, ManagedBlocks, ManagedError, Outcome};
use super::{Registry, blocking};
use crate::protocol::{HooksOutcome, HooksReport, Response};

/// The name of the Managed block holding the hooks.
pub(super) const BLOCK: &str = "worktrunk";
/// The project config of Worktrunk, relative to the root of a Worktree.
pub(crate) const CONFIG: &str = ".config/wt.toml";
/// The hooks Worktrunk runs and the Procly command each one runs.
const HOOKS: [(&str, &str); 2] = [
    ("pre-start", "procly worktree init"),
    ("pre-remove", "procly worktree release"),
];

#[derive(Debug, thiserror::Error)]
enum HooksError {
    #[error(transparent)]
    Agent(#[from] AgentError),
    #[error(transparent)]
    Managed(#[from] ManagedError),
    #[error("{file} is not valid TOML, so Procly cannot tell which hooks it sets: {source}")]
    NotToml {
        file: PathBuf,
        #[source]
        source: toml::de::Error,
    },
}

/// Answers a `WorktreeHooks` request. Resolving the Topology runs git and writing a file is blocking, so the work
/// runs on a thread that may block.
pub(super) async fn answer(registry: &Arc<Registry>, cwd: PathBuf) -> Response {
    let registry = Arc::clone(registry);
    blocking("the hooks could not be written", move || Ok(install(&registry, &cwd)?))
        .await
        .map_or_else(|message| Response::Error { message }, Response::WorktreeHooks)
}

/// The lines of the block, one per hook.
pub(super) fn lines() -> Vec<String> {
    HOOKS
        .iter()
        .map(|(hook, command)| format!("{hook} = \"{command}\""))
        .collect()
}

/// Writes the block into the `.config/wt.toml` of the Worktree holding `cwd`, unless a hook is set outside it.
fn install(registry: &Registry, cwd: &Path) -> Result<HooksReport, HooksError> {
    let _installing = registry
        .agent
        .lock()
        .expect("the agent mutex is never poisoned, no code panics while holding it");
    let topology = topology_in(cwd)?;
    let worktree = &topology.worktree.path;
    let file = contained(worktree, &worktree.join(CONFIG))?;
    let lines = lines();
    // A block that never closes is one to leave alone, its lines are not hooks of the developer. Nor is one moved
    // under a table header, whose keys no longer are the hooks.
    let content = ManagedBlocks::content(&file, BLOCK)?;
    if content == Content::Unclosed || matches!(content, Content::Closed(_)) && !hooks_at_root(&file)? {
        return Ok(HooksReport {
            file,
            outcome: HooksOutcome::Edited,
            set: Vec::new(),
            lines,
        });
    }
    let set = match ManagedBlocks::outside(&file, BLOCK)? {
        Some(text) => set_in(&text).map_err(|source| HooksError::NotToml {
            file: file.clone(),
            source,
        })?,
        None => Vec::new(),
    };
    if !set.is_empty() {
        return Ok(HooksReport {
            file,
            outcome: HooksOutcome::Conflict,
            set,
            lines,
        });
    }
    let outcome = match registry.managed.write(&file, BLOCK, &lines)? {
        Outcome::Written => HooksOutcome::Written,
        Outcome::Unchanged => HooksOutcome::Unchanged,
        Outcome::Edited => HooksOutcome::Edited,
    };
    Ok(HooksReport {
        file,
        outcome,
        set,
        lines,
    })
}

/// Whether the TOML of `file` sets each hook of the block to its Procly command at the root, where Worktrunk reads it.
/// A block moved under a table header holds the same lines, but its keys belong to that table and hook nothing. `true`
/// for a file that is not TOML, which `worktree hooks` reports on its own.
fn hooks_at_root(file: &Path) -> Result<bool, ManagedError> {
    let Some(text) = managed::read_existing(file)? else {
        return Ok(true);
    };
    let Ok(config) = text.parse::<toml::Table>() else {
        return Ok(true);
    };
    Ok(HOOKS
        .iter()
        .all(|(hook, command)| config.get(*hook).and_then(toml::Value::as_str) == Some(*command)))
}

/// The hooks `text` sets, as a key or as a table, which is what Worktrunk reads whatever the spelling of the key.
fn set_in(text: &str) -> Result<Vec<String>, toml::de::Error> {
    let config: toml::Table = text.parse()?;
    Ok(HOOKS
        .iter()
        .map(|(hook, _)| *hook)
        .filter(|hook| config.contains_key(*hook))
        .map(str::to_owned)
        .collect())
}

/// What `init` suggests for the Worktree `worktree`: the hooks, when it has a Worktrunk config that holds no block of
/// Procly yet. `None` for a Worktree without one, and for one that already runs Procly.
pub(crate) fn suggestion(worktree: &Path) -> Option<String> {
    let file = worktree.join(CONFIG);
    if !file.is_file() || !matches!(ManagedBlocks::content(&file, BLOCK), Ok(Content::Absent)) {
        return None;
    }
    Some(format!(
        "{CONFIG} is the Worktrunk config of this Worktree, `procly worktree hooks` makes Worktrunk run \
         `procly worktree init` when it creates a Worktree and `procly worktree release` before it removes one"
    ))
}

/// The content that counts as Procly's whatever the record says, such as the block a Procly whose state was lost wrote.
pub(super) fn known() -> [String; 1] {
    [managed::joined(&lines())]
}

/// Where the block of `file` stands against what Procly recorded. Reads the file and the record, writes neither.
pub(super) fn standing(managed: &ManagedBlocks, file: &Path) -> Result<Standing, ManagedError> {
    let recorded = managed.files_holding(BLOCK)?.iter().any(|recorded| recorded == file);
    Ok(match ManagedBlocks::content(file, BLOCK)? {
        Content::Absent if recorded => Standing::Missing,
        Content::Absent => Standing::Absent,
        Content::Unclosed => Standing::Edited,
        Content::Closed(content) => {
            // A block with no record is intact when it holds what Procly writes today.
            let intact = if recorded {
                managed.is_recorded(file, BLOCK, &content)?
            } else {
                known().contains(&content)
            };
            // The lines of an intact block count for nothing under a table header, where they are not the hooks.
            if intact && hooks_at_root(file)? {
                Standing::Intact
            } else {
                Standing::Edited
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_block_runs_init_before_the_worktree_starts_and_release_before_it_goes() {
        assert_eq!(
            lines(),
            [
                "pre-start = \"procly worktree init\"",
                "pre-remove = \"procly worktree release\""
            ]
        );
    }

    #[test]
    fn a_hook_set_as_a_key_a_quoted_key_or_a_table_is_set() {
        for (text, expected) in [
            ("pre-start = \"npm ci\"\n", vec!["pre-start"]),
            ("\"pre-remove\" = \"make clean\"\n", vec!["pre-remove"]),
            ("[pre-start]\nnpm = \"npm ci\"\n", vec!["pre-start"]),
            (
                "pre-start.npm = \"npm ci\"\n[pre-remove]\nx = \"y\"\n",
                vec!["pre-start", "pre-remove"],
            ),
        ] {
            assert_eq!(set_in(text).unwrap(), expected, "{text}");
        }
    }

    #[test]
    fn other_hooks_and_keys_of_the_same_name_in_a_table_are_not_set() {
        for text in ["", "post-create = \"npm ci\"\n", "[post-start]\npre-start = \"x\"\n"] {
            assert_eq!(set_in(text).unwrap(), Vec::<String>::new(), "{text}");
        }
    }

    #[test]
    fn a_text_that_is_not_toml_is_an_error() {
        assert!(set_in("pre-start = \n[[[").is_err());
    }
}
