//! The record of every container Procly started or adopted for a Compose service, kept in a document of the state
//! directory, so that the next Daemon still counts the Worktrees that need it and resumes its Grace period after a
//! crash or a restart (ADR 0012).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use super::store::{self, StoreError};

/// The version of the format of the container document, bumped when a field changes meaning or disappears.
const DOCUMENT_VERSION: u32 = 1;

/// One running container, as much as a Daemon that did not start it needs to count it and stop it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ContainerRecord {
    /// The common git directory identifying the Repository.
    pub(crate) repository: PathBuf,
    /// The name of the Service in `procly.toml`.
    pub(crate) service: String,
    /// The service of the compose file the Service maps to.
    pub(crate) compose: String,
    /// The compose project the container belongs to, named in its labels.
    pub(crate) project: String,
    /// The Main worktree, where the compose file is read from.
    pub(crate) project_dir: PathBuf,
    /// What `docker compose` needs to reach the engine, and the variables Procly injected for the compose file to
    /// interpolate. The rest of the environment of the caller is never written, it may carry secrets.
    pub(crate) env: Vec<(String, String)>,
    /// The port its health check probes.
    pub(crate) port: Option<u16>,
    /// How long it keeps running once no Worktree needs it.
    pub(crate) grace_ms: u64,
    /// The root of every Worktree that needs it.
    pub(crate) worktrees: Vec<PathBuf>,
    /// When its running Grace period ends, in milliseconds since the Unix epoch, so that it still compares once the
    /// Daemon restarted.
    pub(crate) deadline_ms: Option<u64>,
}

impl ContainerRecord {
    /// The Repository and the name of the Service the container runs for.
    pub(crate) fn key(&self) -> Key {
        (self.repository.clone(), self.service.clone())
    }
}

/// A container is shared by the Worktrees of one Repository, so it is keyed by the Repository and the name of its
/// Service.
pub(crate) type Key = (PathBuf, String);

/// The document holding every record.
#[derive(Debug, Serialize, Deserialize)]
struct ContainerDocument {
    containers: Vec<ContainerRecord>,
}

/// The records of the running containers, written to their document on every change.
#[derive(Debug)]
pub(crate) struct ContainerRecords {
    path: PathBuf,
    /// Held while the document is written, so that it always ends with the latest table.
    table: Mutex<BTreeMap<Key, ContainerRecord>>,
}

impl ContainerRecords {
    /// The records stored in the document at `path`, empty until [`ContainerRecords::replace`] fills them.
    pub(crate) fn new(path: PathBuf) -> Self {
        Self {
            path,
            table: Mutex::default(),
        }
    }

    /// The records a previous Daemon left in the document. Blocking.
    pub(crate) fn load(&self) -> Result<Vec<ContainerRecord>, StoreError> {
        Ok(store::load::<ContainerDocument>(&self.path, DOCUMENT_VERSION)?
            .map_or_else(Vec::new, |document| document.containers))
    }

    /// Replaces every record by `records`, and the document with them. Blocking.
    pub(crate) fn replace(&self, records: Vec<ContainerRecord>) {
        let mut table = self.table();
        *table = records.into_iter().map(|record| (record.key(), record)).collect();
        self.write(&table);
    }

    /// Records the container of the Service `key` as `current` tells it, or forgets it when `current` finds it no
    /// longer runs. `current` reads the container under the lock of the table, so that of 2 updates racing each
    /// other, the document always ends with the later state. Blocking.
    pub(crate) fn update(&self, key: Key, current: impl FnOnce() -> Option<ContainerRecord>) {
        let mut table = self.table();
        let changed = match current() {
            Some(record) if table.get(&key) != Some(&record) => {
                table.insert(key, record);
                true
            }
            Some(_) => false,
            None => table.remove(&key).is_some(),
        };
        if changed {
            self.write(&table);
        }
    }

    /// A container that could not be recorded keeps running, the next Daemon would only lose count of it.
    fn write(&self, table: &BTreeMap<Key, ContainerRecord>) {
        let document = ContainerDocument {
            containers: table.values().cloned().collect(),
        };
        if let Err(error) = store::save(&self.path, DOCUMENT_VERSION, &document) {
            let cause = std::error::Error::source(&error)
                .map(ToString::to_string)
                .unwrap_or_default();
            eprintln!("{error}: {cause}");
        }
    }

    fn table(&self) -> std::sync::MutexGuard<'_, BTreeMap<Key, ContainerRecord>> {
        self.table
            .lock()
            .expect("the container record mutex is never poisoned, no code panics while holding it")
    }
}
