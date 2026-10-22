//! The Repositories that are Off: Guards and Rules blocks stand aside for them, and nothing is removed.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use super::store::{self, StoreError};

/// The version of the format of the document, bumped when a field changes meaning or disappears.
const DOCUMENT_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
struct OffDocument {
    /// The common git directory of each Repository that is Off.
    repositories: Vec<PathBuf>,
}

/// Whether the document at `path` can be read, as the next request that needs it reads it. Nothing is kept or written.
pub(super) fn readable(path: &Path) -> Result<(), StoreError> {
    store::load::<OffDocument>(path, DOCUMENT_VERSION).map(|_| ())
}

/// Which Repositories are Off, kept in a document of the state directory so that it survives restarts of the Daemon.
#[derive(Debug)]
pub(crate) struct Offs {
    path: PathBuf,
    /// Read from the document on the first request, as the Leases are. `None` until then.
    repositories: Mutex<Option<BTreeSet<PathBuf>>>,
}

impl Offs {
    /// The Repositories stored in the document at `path`.
    pub(crate) fn new(path: PathBuf) -> Self {
        Self {
            path,
            repositories: Mutex::new(None),
        }
    }

    /// Whether the Repository whose common git directory is `git_common_dir` is Off.
    pub(crate) fn is_off(&self, git_common_dir: &Path) -> Result<bool, StoreError> {
        self.with(|repositories| Ok(repositories.contains(git_common_dir)))
    }

    /// Switches the Repository Off or back On, and tells whether that changed anything.
    pub(crate) fn set(&self, git_common_dir: &Path, off: bool) -> Result<bool, StoreError> {
        self.with(|repositories| {
            let mut updated = repositories.clone();
            let changed = if off {
                updated.insert(git_common_dir.to_owned())
            } else {
                updated.remove(git_common_dir)
            };
            if changed {
                store::save(
                    &self.path,
                    DOCUMENT_VERSION,
                    &OffDocument {
                        repositories: updated.iter().cloned().collect(),
                    },
                )?;
                *repositories = updated;
            }
            Ok(changed)
        })
    }

    fn with<T>(&self, action: impl FnOnce(&mut BTreeSet<PathBuf>) -> Result<T, StoreError>) -> Result<T, StoreError> {
        let mut repositories = self
            .repositories
            .lock()
            .expect("the off table mutex is never poisoned, no code panics while holding it");
        if repositories.is_none() {
            let document = store::load::<OffDocument>(&self.path, DOCUMENT_VERSION)?;
            *repositories = Some(
                document
                    .map(|document| document.repositories.into_iter().collect())
                    .unwrap_or_default(),
            );
        }
        let repositories = repositories
            .as_mut()
            .expect("the table was read just above if it was not yet");
        action(repositories)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_repository_is_off_until_it_is_switched_back_on_and_a_new_daemon_remembers_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("off.json");
        let repository = Path::new("/p/work/acme/.git");
        let offs = Offs::new(path.clone());

        assert!(!offs.is_off(repository).unwrap());
        assert!(offs.set(repository, true).unwrap());
        assert!(!offs.set(repository, true).unwrap(), "already off");
        assert!(offs.is_off(repository).unwrap());
        assert!(!offs.is_off(Path::new("/p/work/globex/.git")).unwrap());

        let restarted = Offs::new(path);
        assert!(restarted.is_off(repository).unwrap());
        assert!(restarted.set(repository, false).unwrap());
        assert!(!restarted.is_off(repository).unwrap());
        assert!(!restarted.set(repository, false).unwrap(), "already on");
    }
}
