//! Topology: the Repository and the Worktree a directory belongs to, discovered through `git` (ADR 0011).

use std::ffi::OsStr;
use std::io;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

/// The longest Worktree slug, leaving room for a prefix and a suffix in a 63-byte Postgres identifier.
const SLUG_MAX_LEN: usize = 32;

#[derive(Debug, thiserror::Error)]
pub(crate) enum TopologyError {
    #[error("cannot run git in {dir}")]
    Spawn {
        dir: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("git {args} failed in {dir}: {stderr}")]
    Git { dir: PathBuf, args: String, stderr: String },
    #[error("git {args} printed an unexpected {output:?} in {dir}")]
    UnexpectedOutput { dir: PathBuf, args: String, output: String },
    #[error("{dir} is not in a git repository")]
    NotARepository { dir: PathBuf },
    #[error("cannot resolve {path}")]
    Canonicalize {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Where a directory sits: the Repository, identified by its common git directory, and the Worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Topology {
    pub(crate) git_common_dir: PathBuf,
    pub(crate) worktree: Worktree,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Worktree {
    /// The root directory of the Worktree, canonicalized.
    pub(crate) path: PathBuf,
    pub(crate) slug: String,
    /// Whether this is the Main worktree, the first entry of `git worktree list`.
    pub(crate) main: bool,
}

/// The Topology of a directory, or `None` when it is not inside a git Repository.
pub(crate) fn resolve(dir: &Path) -> Result<Option<Topology>, TopologyError> {
    let args = [
        "rev-parse",
        "--path-format=absolute",
        "--git-common-dir",
        "--show-toplevel",
    ];
    let output = match git(dir, &args) {
        Ok(output) => output,
        Err(TopologyError::NotARepository { .. }) => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut lines = output
        .split(|&byte| byte == b'\n')
        .map(|line| PathBuf::from(OsStr::from_bytes(line)));
    let (Some(git_common_dir), Some(toplevel)) = (lines.next(), lines.next()) else {
        return Err(TopologyError::UnexpectedOutput {
            dir: dir.to_owned(),
            args: args.join(" "),
            output: String::from_utf8_lossy(&output).into_owned(),
        });
    };
    let git_common_dir = canonicalize(&git_common_dir)?;
    let toplevel = canonicalize(&toplevel)?;
    let worktree = worktrees(&git_common_dir)?
        .into_iter()
        .find(|worktree| worktree.path == toplevel)
        // Only a Worktree added after the list was read is missing from it, and the Main worktree is listed first.
        .unwrap_or_else(|| Worktree {
            slug: slug_of(&toplevel),
            path: toplevel,
            main: false,
        });
    Ok(Some(Topology {
        git_common_dir,
        worktree,
    }))
}

/// Every Worktree of a Repository, in the order of `git worktree list`.
///
/// The first entry of a bare Repository is the bare directory itself, which is no Worktree and is skipped, so no
/// Worktree of a bare Repository is the Main worktree.
pub(crate) fn worktrees(git_common_dir: &Path) -> Result<Vec<Worktree>, TopologyError> {
    let listed = parse_worktree_list(&git(git_common_dir, &["worktree", "list", "--porcelain", "-z"])?);
    let main = listed.first().map(|entry| entry.path.clone());
    Ok(listed
        .into_iter()
        .filter(|entry| !entry.bare && !entry.prunable)
        .map(|entry| {
            let is_main = main.as_ref() == Some(&entry.path);
            // git records the path as given to `worktree add`, which may go through a symlink.
            let path = std::fs::canonicalize(&entry.path).unwrap_or(entry.path);
            Worktree {
                slug: slug_of(&path),
                path,
                main: is_main,
            }
        })
        .collect())
}

/// Runs `git` in `dir`, ignoring the `GIT_*` variables the Daemon inherited from whichever command started it,
/// with messages in English so that a directory outside any Repository is recognized.
fn git(dir: &Path, args: &[&str]) -> Result<Vec<u8>, TopologyError> {
    let mut command = Command::new("git");
    for (name, _) in std::env::vars_os() {
        if name.as_bytes().starts_with(b"GIT_") {
            command.env_remove(name);
        }
    }
    let output = command
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("LC_ALL", "C")
        .output()
        .map_err(|source| TopologyError::Spawn {
            dir: dir.to_owned(),
            source,
        })?;
    if output.status.success() {
        let mut stdout = output.stdout;
        if stdout.last() == Some(&b'\n') {
            stdout.pop();
        }
        return Ok(stdout);
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if stderr.contains("not a git repository") {
        Err(TopologyError::NotARepository { dir: dir.to_owned() })
    } else {
        Err(TopologyError::Git {
            dir: dir.to_owned(),
            args: args.join(" "),
            stderr,
        })
    }
}

pub(crate) fn canonicalize(path: &Path) -> Result<PathBuf, TopologyError> {
    std::fs::canonicalize(path).map_err(|source| TopologyError::Canonicalize {
        path: path.to_owned(),
        source,
    })
}

fn slug_of(worktree: &Path) -> String {
    slug(&worktree.file_name().unwrap_or_default().to_string_lossy())
}

/// The Worktree slug of a Worktree directory name: lowercase ASCII letters and digits, any other run of
/// characters folded into one `_`, so it embeds unquoted in database, cookie and compose project names.
pub(crate) fn slug(dir_name: &str) -> String {
    let mut slug = String::new();
    for c in dir_name.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('_') {
            slug.push('_');
        }
    }
    // Only ASCII is pushed, so any byte index is a character boundary.
    slug.truncate(SLUG_MAX_LEN);
    let slug = slug.trim_end_matches('_');
    if slug.is_empty() {
        "worktree".to_owned()
    } else {
        slug.to_owned()
    }
}

/// One entry of `git worktree list --porcelain -z`.
#[derive(Debug, PartialEq, Eq)]
struct ListedWorktree {
    path: PathBuf,
    /// The entry of a bare Repository, which is no Worktree.
    bare: bool,
    /// A Worktree whose directory is gone, waiting for `git worktree prune`.
    prunable: bool,
}

/// Parses NUL-separated attribute lines, with an empty field closing each entry, so any path survives.
fn parse_worktree_list(output: &[u8]) -> Vec<ListedWorktree> {
    let mut listed = Vec::new();
    let mut current: Option<ListedWorktree> = None;
    for field in output.split(|&byte| byte == 0) {
        if let Some(path) = field.strip_prefix(b"worktree ") {
            listed.extend(current.take());
            current = Some(ListedWorktree {
                path: PathBuf::from(OsStr::from_bytes(path)),
                bare: false,
                prunable: false,
            });
        } else if let Some(entry) = current.as_mut() {
            if field == b"bare" {
                entry.bare = true;
            } else if field.starts_with(b"prunable") {
                entry.prunable = true;
            }
        }
    }
    listed.extend(current);
    listed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_worktree_list_keeps_every_entry_in_order_with_its_flags() {
        let output = b"worktree /src/app.git\0bare\0\0\
                       worktree /src/app main\0HEAD f181\0branch refs/heads/main\0\0\
                       worktree /src/gone\0HEAD f181\0detached\0prunable gitdir file points to non-existent location\0\0";
        let listed = |path: &str, bare, prunable| ListedWorktree {
            path: PathBuf::from(path),
            bare,
            prunable,
        };
        assert_eq!(
            parse_worktree_list(output),
            [
                listed("/src/app.git", true, false),
                listed("/src/app main", false, false),
                listed("/src/gone", false, true),
            ]
        );
    }

    #[test]
    fn a_slug_keeps_lowercase_letters_and_digits() {
        assert_eq!(slug("app2"), "app2");
    }

    #[test]
    fn a_slug_lowercases_and_folds_every_other_run_into_one_underscore() {
        assert_eq!(slug("Feature X--Login.v2"), "feature_x_login_v2");
        assert_eq!(slug("café-crème"), "caf_cr_me");
    }

    #[test]
    fn a_slug_has_no_leading_or_trailing_separator() {
        assert_eq!(slug("--fix-123--"), "fix_123");
    }

    #[test]
    fn a_slug_is_never_empty() {
        assert_eq!(slug(""), "worktree");
        assert_eq!(slug("..."), "worktree");
    }

    #[test]
    fn a_slug_is_cut_to_32_bytes_without_a_trailing_separator() {
        assert_eq!(
            slug("a-very-long-worktree-name-for-a-feature-branch"),
            "a_very_long_worktree_name_for_a"
        );
        assert_eq!(slug(&"x".repeat(40)), "x".repeat(32));
    }
}
