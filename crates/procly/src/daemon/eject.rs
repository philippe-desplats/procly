//! Eject: stops the Services of a Repository and takes out of it everything Procly wrote (ADR 0008).
//!
//! The Managed blocks nobody edited, the Guard, the exclusions and the Env files are removed mechanically. A block
//! the developer edited stays where it is and is described in a prompt, which `--agent` hands to their own coding
//! agent. Docker volumes and Logical resources are never touched, containers are stopped and not removed.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::agent::{INSTRUCTION_FILES, RULES, SETTINGS, contained, rules};
use super::compose::ComposeError;
use super::environment::{ENV_BLOCK, ENV_FILE, EXCLUDE_FILE, EXCLUSIONS, GITIGNORE_FILE, exclusion_lines};
use super::hook::{self, HookError};
use super::managed::{self, ManagedBlocks, ManagedError, Removal};
use super::{Registry, failure, requester};
use crate::config;
use crate::envfile;
use crate::protocol::{EjectReport, Left, Removed, Response, Stopped, Written};
use crate::topology::{self, Topology, TopologyError, Worktree};

#[derive(Debug, thiserror::Error)]
enum EjectError {
    #[error(transparent)]
    Topology(#[from] TopologyError),
    #[error(transparent)]
    Managed(#[from] ManagedError),
    #[error(transparent)]
    Hook(#[from] HookError),
    #[error(transparent)]
    Compose(Box<ComposeError>),
    #[error("the eject could not be finished: {0}")]
    Interrupted(String),
    #[error("cannot read {path}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cannot remove {path}")]
    Remove {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Answers an `Eject` request for the Repository holding `cwd`. Services are stopped first, so that nothing writes an
/// Env file again once it is removed. Stopping fails before any file is touched, and asking again finishes the job.
pub(super) async fn run(registry: &Arc<Registry>, cwd: PathBuf, peer: Option<i32>) -> Response {
    eject(registry, cwd, peer).await.map_or_else(failure, Response::Eject)
}

async fn eject(registry: &Arc<Registry>, cwd: PathBuf, peer: Option<i32>) -> Result<EjectReport, EjectError> {
    // Resolving the Topology runs git, and naming the requester runs ps.
    let (topology, worktrees, who) = tokio::task::spawn_blocking(move || -> Result<_, EjectError> {
        let topology = topology_in(&cwd)?;
        let worktrees = topology::worktrees(&topology.git_common_dir)?;
        Ok((topology, worktrees, requester::describe(peer, &cwd)))
    })
    .await
    .map_err(|error| EjectError::Interrupted(error.to_string()))??;
    eprintln!("eject of {} requested by {who}", topology.git_common_dir.display());

    let mut report = EjectReport::default();
    for ((worktree, service), process) in registry.services.stop_repository(&topology.git_common_dir) {
        process.ended().await;
        report.stopped.push(Stopped {
            service,
            worktree: Some(worktree),
        });
    }
    for service in registry
        .composes
        .stop_repository(&topology.git_common_dir)
        .await
        .map_err(|error| EjectError::Compose(Box::new(error)))?
    {
        report.stopped.push(Stopped {
            service,
            worktree: None,
        });
    }

    let registry = Arc::clone(registry);
    let git_common_dir = topology.git_common_dir;
    let (removed, left, prompt) = tokio::task::spawn_blocking(move || {
        let (removed, left) = remove_written(&registry, &git_common_dir, &worktrees)?;
        let prompt = prompt(&git_common_dir, &left);
        Ok::<_, EjectError>((removed, left, prompt))
    })
    .await
    .map_err(|error| EjectError::Interrupted(error.to_string()))??;
    report.removed = removed;
    report.left = left;
    report.prompt = prompt;
    Ok(report)
}

/// The Repository and the Worktree holding `cwd`, which must be in a git Repository.
fn topology_in(cwd: &Path) -> Result<Topology, TopologyError> {
    let cwd = topology::canonicalize(cwd)?;
    topology::resolve(&cwd)?.ok_or(TopologyError::NotARepository { dir: cwd })
}

/// Removes what Procly wrote into the Repository, and returns what was removed and what was left because it was edited.
fn remove_written(
    registry: &Registry,
    git_common_dir: &Path,
    worktrees: &[Worktree],
) -> Result<(Vec<Removed>, Vec<Left>), EjectError> {
    // Neither an install nor a start may write while the files are taken out.
    let _installing = registry
        .agent
        .lock()
        .expect("the agent mutex is never poisoned, no code panics while holding it");
    let _mirroring = registry
        .mirroring
        .lock()
        .expect("the mirroring mutex is never poisoned, no code panics while holding it");
    let places = Places {
        git_common_dir,
        worktrees,
    };
    let mut outcome = Outcome::default();

    // The contents Procly writes today count as its own even when the fingerprint was lost with the state directory.
    let known_rules = [managed::joined(rules(false)), managed::joined(rules(true))];
    for file in places.files(&registry.managed, RULES, |worktree| {
        INSTRUCTION_FILES.iter().map(|name| worktree.join(name)).collect()
    })? {
        let removal = registry.managed.remove(&file, RULES, &known_rules)?;
        outcome.record(&places, Written::Rules, file, removal);
    }

    for file in places.files(&registry.managed, hook::BLOCK, |worktree| vec![worktree.join(SETTINGS)])? {
        let removal = hook::remove(&registry.managed, &file)?;
        outcome.record(&places, Written::Guard, file, removal);
    }

    let known_exclusions = [managed::joined(&exclusion_lines())];
    let exclude = git_common_dir.join(EXCLUDE_FILE);
    for file in places.files(&registry.managed, EXCLUSIONS, |worktree| {
        vec![worktree.join(GITIGNORE_FILE), exclude.clone()]
    })? {
        let removal = registry.managed.remove(&file, EXCLUSIONS, &known_exclusions)?;
        outcome.record(&places, Written::Exclusions, file, removal);
    }

    for worktree in worktrees {
        let file = worktree.path.join(ENV_FILE);
        let text = match fs::read_to_string(&file) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(source) => return Err(EjectError::Read { path: file, source }),
        };
        // Intact while it starts with its header and holds what Procly last wrote, a line added since is an edit.
        let edited = !envfile::is_generated(&text) || registry.managed.departs_from_record(&file, ENV_BLOCK, &text)?;
        if edited {
            outcome.left.push(Left {
                kind: Written::EnvFile,
                file,
            });
        } else {
            fs::remove_file(&file).map_err(|source| EjectError::Remove {
                path: file.clone(),
                source,
            })?;
            registry.managed.forget_block(&file, ENV_BLOCK)?;
            outcome.removed.push(Removed {
                kind: Written::EnvFile,
                file,
                deleted: true,
            });
        }
    }
    Ok((outcome.removed, outcome.left))
}

#[derive(Debug, Default)]
struct Outcome {
    removed: Vec<Removed>,
    left: Vec<Left>,
}

impl Outcome {
    fn record(&mut self, places: &Places, kind: Written, file: PathBuf, removal: Removal) {
        match removal {
            Removal::Removed { deleted } => {
                if deleted {
                    places.prune_empty_directories(&file);
                }
                self.removed.push(Removed { kind, file, deleted });
            }
            Removal::Edited => self.left.push(Left { kind, file }),
            Removal::Absent => {}
        }
    }
}

/// Where Procly may have written: the Worktrees of the Repository and its git directory.
struct Places<'a> {
    git_common_dir: &'a Path,
    worktrees: &'a [Worktree],
}

impl Places<'_> {
    /// The files that may hold the block `block`: the ones Procly recorded it in, and the ones it writes by default,
    /// when they exist. A file that links out of the Repository is never one of them, since nothing is written there.
    fn files(
        &self,
        managed: &ManagedBlocks,
        block: &str,
        defaults: impl Fn(&Path) -> Vec<PathBuf>,
    ) -> Result<BTreeSet<PathBuf>, EjectError> {
        let mut files = BTreeSet::new();
        let recorded = managed.files_holding(block)?;
        let defaults = self.worktrees.iter().flat_map(|worktree| defaults(&worktree.path));
        for file in recorded.into_iter().chain(defaults) {
            if let Some(inside) = self.inside(&file) {
                files.insert(inside);
            }
        }
        Ok(files)
    }

    /// `file` with its symlinks resolved, when it exists and sits inside the Repository.
    fn inside(&self, file: &Path) -> Option<PathBuf> {
        if file.starts_with(self.git_common_dir) {
            return file
                .canonicalize()
                .ok()
                .filter(|file| file.starts_with(self.git_common_dir));
        }
        let worktree = self.worktree_of(file)?;
        file.exists().then(|| contained(&worktree.path, file).ok()).flatten()
    }

    fn worktree_of(&self, file: &Path) -> Option<&Worktree> {
        self.worktrees.iter().find(|worktree| file.starts_with(&worktree.path))
    }

    /// Removes the directories a deleted file leaves empty, up to the root of its Worktree, which stays.
    fn prune_empty_directories(&self, file: &Path) {
        let Some(root) = self.worktree_of(file).map(|worktree| worktree.path.as_path()) else {
            return;
        };
        let mut directory = file.parent();
        while let Some(current) = directory.filter(|current| *current != root && current.starts_with(root)) {
            // Fails while the directory still holds something, which ends the climb.
            if fs::remove_dir(current).is_err() {
                break;
            }
            directory = current.parent();
        }
    }
}

/// What to undo by hand in the items eject left, written for a coding agent. `None` when it left nothing.
fn prompt(git_common_dir: &Path, left: &[Left]) -> Option<String> {
    if left.is_empty() {
        return None;
    }
    let mut text = format!(
        "Procly was ejected from the git repository whose common git directory is {}. It removed everything it wrote \
         that nobody edited. The items below were edited after Procly wrote them, so it left them as they are. In \
         each one, undo what Procly added and keep every change that is yours. Touch nothing else, and do not run \
         any procly command.\n",
        git_common_dir.display()
    );
    for (number, item) in left.iter().enumerate() {
        let _ = write!(text, "\n{}. {}: {}\n", number + 1, item.file.display(), undo(item));
    }
    Some(text)
}

/// What to undo in one item eject left.
fn undo(item: &Left) -> String {
    let markers = |block| managed::markers(&item.file, block);
    match item.kind {
        Written::Rules => {
            let (open, close) = markers(RULES);
            format!(
                "the Rules block. Delete the line `{open}`, the line `{close}` and the instructions Procly wrote between \
             them, which tell agents to start Services with `procly up`, stop them with `procly down`, read \
             `procly status`, and not to start dev servers or containers themselves. Keep every line that is yours, \
             wherever it sits."
            )
        }
        Written::Guard => format!(
            "the Guard. It is the hook of `hooks.PreToolUse` whose command is `{}`. Delete that hook, and its group \
             when it holds nothing else, then delete `hooks.PreToolUse` and `hooks` if they end up empty. Keep every \
             other hook and setting.",
            hook::COMMAND
        ),
        Written::Exclusions => {
            let (open, close) = markers(EXCLUSIONS);
            let [env_file, local_file] = exclusion_lines();
            format!(
                "the Managed block that excludes the files Procly generates from git. Delete the line `{open}`, the \
                 line `{close}` and the lines `{env_file}` and `{local_file}` between them. Keep every line that is \
                 yours."
            )
        }
        Written::EnvFile => format!(
            "the Env file Procly generated, which no longer starts with its header. Delete the variables Procly \
             injected, which are the Derived variables declared under `env` in `{}`, and delete the file when \
             nothing else remains in it.",
            config::VERSIONED_FILE
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn left(kind: Written, file: &str) -> Left {
        Left {
            kind,
            file: PathBuf::from(file),
        }
    }

    #[test]
    fn nothing_left_needs_no_prompt() {
        assert_eq!(prompt(Path::new("/p/work/acme/.git"), &[]), None);
    }

    #[test]
    fn the_prompt_numbers_every_item_left_with_its_file_and_what_to_undo() {
        let items = [
            left(Written::Rules, "/p/work/acme/docs/agents.md"),
            left(Written::Guard, "/p/work/acme/.claude/settings.json"),
            left(Written::Exclusions, "/p/work/acme/.gitignore"),
        ];

        let text = prompt(Path::new("/p/work/acme/.git"), &items).expect("items were left");

        assert!(text.contains("/p/work/acme/.git"), "{text}");
        assert!(
            text.contains(
                "1. /p/work/acme/docs/agents.md: the Rules block. Delete the line `<!-- >>> procly managed block: \
                 rules >>> -->`"
            ),
            "{text}"
        );
        assert!(
            text.contains("2. /p/work/acme/.claude/settings.json: the Guard. It is the hook of `hooks.PreToolUse` whose command is `procly guard`"),
            "{text}"
        );
        assert!(
            text.contains("3. /p/work/acme/.gitignore: the Managed block that excludes")
                && text.contains("`# >>> procly managed block: exclusions >>>`"),
            "{text}"
        );
        assert!(text.contains("do not run any procly command"), "{text}");
    }

    #[test]
    fn the_prompt_holds_no_em_dash_nor_semicolon() {
        let items = [
            left(Written::Rules, "/a/AGENTS.md"),
            left(Written::Guard, "/a/.claude/settings.json"),
            left(Written::Exclusions, "/a/.gitignore"),
            left(Written::EnvFile, "/a/.env.procly"),
        ];

        let text = prompt(Path::new("/a/.git"), &items).expect("items were left");

        assert!(!text.contains(['\u{2014}', '\u{2013}', ';']), "{text}");
    }
}
