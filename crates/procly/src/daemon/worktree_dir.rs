//! The directories of the state directory that belong to one Worktree, such as its output files and its setup log.
//! Each one records the Repository and the Worktree it belongs to, so that `procly gc` tells an Orphan from a
//! directory it cannot place, which it never deletes (ADR 0004).

use std::fs::{self, DirBuilder};
use std::io;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::atomic_file::{self, Mode};
use super::fnv;
use super::store::{self, StoreError};
use crate::topology::Topology;

/// The record of the Worktree a directory belongs to, inside that directory.
pub(crate) const RECORD_FILE: &str = "worktree.json";
/// The version of the format of the record, a state document like the others (ADR 0012).
const RECORD_VERSION: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub(crate) enum WorktreeDirError {
    #[error("cannot create {path}")]
    Create {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(transparent)]
    Record(#[from] StoreError),
}

/// The Repository and the Worktree a directory belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Owner {
    /// The common git directory identifying the Repository.
    pub(crate) repository: PathBuf,
    /// The root of the Worktree, canonicalized.
    pub(crate) worktree: PathBuf,
}

/// A directory of one Worktree, with its owner when its record can be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorktreeDir {
    pub(crate) path: PathBuf,
    /// `None` for a directory without a readable record, which nothing can tell the Worktree of.
    pub(crate) owner: Option<Owner>,
}

/// The directory of the Worktree of `topology` in `parent`, created readable by the user only with its record when
/// it does not exist yet. Blocking.
pub(crate) fn create(parent: &Path, topology: &Topology) -> Result<PathBuf, WorktreeDirError> {
    let path = parent.join(name(&topology.worktree.path, &topology.worktree.slug));
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&path)
        .map_err(|source| WorktreeDirError::Create {
            path: path.clone(),
            source,
        })?;
    let record = path.join(RECORD_FILE);
    if !record.exists() {
        let owner = Owner {
            repository: topology.git_common_dir.clone(),
            worktree: topology.worktree.path.clone(),
        };
        store::save(&record, RECORD_VERSION, &owner)?;
    }
    Ok(path)
}

/// Every directory in `parent`, with the Worktree it belongs to. Files are skipped, and so is a `parent` that does not
/// exist. Blocking.
pub(crate) fn list(parent: &Path) -> Vec<WorktreeDir> {
    let Ok(entries) = fs::read_dir(parent) else {
        return Vec::new();
    };
    let mut dirs: Vec<WorktreeDir> = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            // Not followed, a symlink is no directory Procly made.
            entry.file_type().ok()?.is_dir().then(|| entry.path())
        })
        .map(|path| {
            let owner = store::load::<Owner>(&path.join(RECORD_FILE), RECORD_VERSION)
                .ok()
                .flatten();
            WorktreeDir { path, owner }
        })
        .collect();
    dirs.sort_by(|a, b| a.path.cmp(&b.path));
    dirs
}

/// Removes the directory at `path` with all it holds, its record last, and puts the record back when the directory
/// itself cannot be removed, so that it still names its Worktree for the next `procly gc --apply`. A directory already
/// gone counts as removed. Blocking.
pub(crate) fn remove(path: &Path) -> io::Result<()> {
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        if entry.file_name() == RECORD_FILE {
            continue;
        }
        // Not followed, a symlink is removed as a file.
        if entry.file_type()?.is_dir() {
            fs::remove_dir_all(entry.path())?;
        } else {
            fs::remove_file(entry.path())?;
        }
    }
    let record = path.join(RECORD_FILE);
    let kept = fs::read(&record).ok();
    match fs::remove_file(&record) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    fs::remove_dir(path).inspect_err(|_| {
        if let Some(kept) = &kept {
            let _ = atomic_file::replace(&record, kept, Mode::Private);
        }
    })
}

/// The name of the directory of the Worktree at `worktree`: its slug, for whoever browses the state directory, then
/// FNV-1a 64 of its root, so that 2 Worktrees with the same slug never share one and every later Procly finds it again.
fn name(worktree: &Path, slug: &str) -> String {
    format!("{slug}-{:016x}", fnv::fnv1a64(worktree.as_os_str().as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::topology::Worktree;
    use std::os::unix::fs::PermissionsExt as _;

    fn topology(worktree: &str) -> Topology {
        Topology {
            git_common_dir: PathBuf::from("/src/app/.git"),
            worktree: Worktree {
                path: PathBuf::from(worktree),
                slug: crate::topology::slug(worktree),
                main: false,
            },
        }
    }

    #[test]
    fn the_name_of_a_directory_is_the_slug_then_fnv_1a_64_of_the_worktree_root() {
        // A later Procly must find the directory again, whatever toolchain built it.
        assert_eq!(name(Path::new("/src/app-a"), "app-a"), "app-a-e28808b1d210e342");
    }

    #[test]
    fn a_directory_records_its_worktree_and_is_removed_with_all_it_holds() {
        let parent = tempfile::tempdir().unwrap();
        let dir = create(parent.path(), &topology("/src/app-a")).unwrap();
        fs::create_dir(dir.join("nested")).unwrap();
        fs::write(dir.join("nested").join("x.log"), "x").unwrap();
        fs::write(dir.join("y.log"), "y").unwrap();

        let listed = list(parent.path());
        assert_eq!(listed.len(), 1);
        assert_eq!(
            listed[0].owner,
            Some(Owner {
                repository: PathBuf::from("/src/app/.git"),
                worktree: PathBuf::from("/src/app-a"),
            })
        );

        remove(&dir).unwrap();
        assert!(!dir.exists());
        remove(&dir).unwrap();
    }

    #[test]
    fn a_directory_that_cannot_be_removed_keeps_its_record() {
        let parent = tempfile::tempdir().unwrap();
        let dir = create(parent.path(), &topology("/src/app-a")).unwrap();
        fs::write(dir.join("y.log"), "y").unwrap();
        fs::set_permissions(parent.path(), fs::Permissions::from_mode(0o500)).unwrap();

        let removed = remove(&dir);

        fs::set_permissions(parent.path(), fs::Permissions::from_mode(0o700)).unwrap();
        assert!(removed.is_err());
        assert!(!dir.join("y.log").exists());
        assert!(list(parent.path())[0].owner.is_some());
    }

    #[test]
    fn a_name_starts_with_the_slug_and_tells_2_worktrees_of_the_same_slug_apart() {
        let (a, b) = (
            name(Path::new("/src/a/app"), "app"),
            name(Path::new("/src/b/app"), "app"),
        );
        assert!(a.starts_with("app-") && b.starts_with("app-"));
        assert_ne!(a, b);
        assert_eq!(a, name(Path::new("/src/a/app"), "app"));
    }
}
