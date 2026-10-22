//! The record of the Logical resources each Worktree other than the Main worktree has in a Logical container, so that
//! `procly gc` finds the ones of a removed Worktree and drops them through their Driver (ADR 0004).
//!
//! A resource is recorded as soon as its name or its index is worked out, before the Driver creates it, so that no
//! index is ever handed out without a record. A resource the Driver never created is found missing by its `check`
//! when `gc` comes, and only forgotten. The record is a document of the state directory (ADR 0012).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use serde::{Deserialize, Serialize};

use super::store::{self, StoreError};

/// The version of the format of the document, bumped when a field changes meaning or disappears.
const DOCUMENT_VERSION: u32 = 1;

/// One resource of one Compose service in one Worktree of one Repository.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct ResourceKey {
    /// The common git directory identifying the Repository.
    pub(crate) repository: PathBuf,
    /// The root of the Worktree.
    pub(crate) worktree: PathBuf,
    pub(crate) service: String,
    /// The key of the resource in the configuration.
    pub(crate) resource: String,
}

/// A Logical resource as the record keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct LogicalResource {
    #[serde(flatten)]
    pub(crate) key: ResourceKey,
    /// Its name in the container, its index for a Driver that numbers its resources.
    pub(crate) name: String,
    /// The Driver that created it and drops it.
    pub(crate) driver: String,
    /// Whether the Driver numbers its resources, so that `name` is an index the Daemon handed out.
    pub(crate) indexed: bool,
    /// The Pinned branch the Worktree was on, `None` when its branch was not pinned.
    pub(crate) branch: Option<String>,
}

type Table = BTreeMap<ResourceKey, LogicalResource>;

#[derive(Debug, Serialize, Deserialize)]
struct ResourceDocument {
    resources: Vec<LogicalResource>,
}

/// Whether the document at `path` can be read, as the next request that needs it reads it. Nothing is kept or written.
pub(super) fn readable(path: &Path) -> Result<(), StoreError> {
    store::load::<ResourceDocument>(path, DOCUMENT_VERSION).map(|_| ())
}

/// Every Logical resource recorded, across all Repositories.
#[derive(Debug)]
pub(crate) struct LogicalResources {
    path: PathBuf,
    /// Read from the document on the first request, so that an unreadable document fails that request, naming the
    /// file, instead of keeping the Daemon from starting. `None` until then.
    table: Mutex<Option<Table>>,
}

impl LogicalResources {
    /// The resources recorded in the document at `path`.
    pub(crate) fn new(path: PathBuf) -> Self {
        Self {
            path,
            table: Mutex::new(None),
        }
    }

    /// Records `resources`, replacing what was recorded under the same keys. Blocking: it writes the document when
    /// something changed.
    pub(crate) fn record(&self, resources: impl IntoIterator<Item = LogicalResource>) -> Result<(), StoreError> {
        self.change(|table| {
            for resource in resources {
                table.insert(resource.key.clone(), resource);
            }
        })
    }

    /// Every recorded resource, in the order of their keys. Blocking: it reads the document the first time.
    pub(crate) fn all(&self) -> Result<Vec<LogicalResource>, StoreError> {
        let mut table = self.table()?;
        Ok(loaded(&mut table).values().cloned().collect())
    }

    /// Forgets the resource `key`. Blocking: it writes the document.
    pub(crate) fn forget(&self, key: &ResourceKey) -> Result<(), StoreError> {
        self.change(|table| {
            table.remove(key);
        })
    }

    /// Applies `change` to the table, storing the result before it becomes the table when it differs.
    fn change(&self, change: impl FnOnce(&mut Table)) -> Result<(), StoreError> {
        let mut guard = self.table()?;
        let table = loaded(&mut guard);
        let mut changed = table.clone();
        change(&mut changed);
        if changed != *table {
            write(&self.path, &changed)?;
            *table = changed;
        }
        Ok(())
    }

    /// The table, read from the document when it was not yet.
    fn table(&self) -> Result<MutexGuard<'_, Option<Table>>, StoreError> {
        let mut table = self
            .table
            .lock()
            .expect("the resource table mutex is never poisoned, no code panics while holding it");
        if table.is_none() {
            *table = Some(read(&self.path)?);
        }
        Ok(table)
    }
}

fn loaded<'a>(table: &'a mut MutexGuard<'_, Option<Table>>) -> &'a mut Table {
    table.as_mut().expect("the resource table is read before it is used")
}

fn read(path: &Path) -> Result<Table, StoreError> {
    let document = store::load::<ResourceDocument>(path, DOCUMENT_VERSION)?;
    Ok(document
        .map(|document| {
            document
                .resources
                .into_iter()
                .map(|resource| (resource.key.clone(), resource))
                .collect()
        })
        .unwrap_or_default())
}

fn write(path: &Path, table: &Table) -> Result<(), StoreError> {
    let resources = table.values().cloned().collect();
    store::save(path, DOCUMENT_VERSION, &ResourceDocument { resources })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resource(worktree: &str, name: &str) -> LogicalResource {
        LogicalResource {
            key: ResourceKey {
                repository: PathBuf::from("/src/app/.git"),
                worktree: PathBuf::from(worktree),
                service: "db".to_owned(),
                resource: "dev".to_owned(),
            },
            name: name.to_owned(),
            driver: "postgres".to_owned(),
            indexed: false,
            branch: None,
        }
    }

    #[test]
    fn resources_survive_the_daemon_that_recorded_them_until_they_are_forgotten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("resources.json");
        let (a, b) = (
            resource("/src/app-a", "app__app_a"),
            resource("/src/app-b", "app__app_b"),
        );
        LogicalResources::new(path.clone())
            .record([a.clone(), b.clone()])
            .unwrap();

        let restarted = LogicalResources::new(path.clone());
        assert_eq!(restarted.all().unwrap(), [a.clone(), b.clone()]);
        restarted.forget(&a.key).unwrap();

        assert_eq!(LogicalResources::new(path).all().unwrap(), [b]);
    }

    #[test]
    fn recording_a_resource_again_replaces_it_and_writes_nothing_when_nothing_changed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("resources.json");
        let resources = LogicalResources::new(path.clone());
        resources.record([resource("/src/app-a", "old")]).unwrap();
        resources.record([resource("/src/app-a", "new")]).unwrap();
        assert_eq!(resources.all().unwrap(), [resource("/src/app-a", "new")]);

        std::fs::remove_file(&path).unwrap();
        resources.record([resource("/src/app-a", "new")]).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn an_unreadable_document_fails_the_request_naming_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("resources.json");
        std::fs::write(&path, "not json").unwrap();
        let error = LogicalResources::new(path).all().unwrap_err();
        assert!(error.to_string().contains("resources.json"), "{error}");
    }
}
