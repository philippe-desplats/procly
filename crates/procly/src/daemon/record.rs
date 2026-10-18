//! The record of every Service the Daemon runs, kept in a document of the state directory, so that the next Daemon
//! finds them after a crash and never starts one of them a second time (ADR 0012).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use super::store::{self, StoreError};
use crate::topology::{Topology, Worktree};

/// The version of the format of the Service document, bumped when a field changes meaning or disappears.
const DOCUMENT_VERSION: u32 = 1;

/// One running Service, as much as a Daemon that did not start it needs to supervise it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ServiceRecord {
    /// The common git directory identifying the Repository.
    pub(crate) repository: PathBuf,
    pub(crate) worktree: Worktree,
    pub(crate) service: String,
    /// The leader of the process group of the current run, so its pid is also the group id.
    pub(crate) pgid: i32,
    /// When the leader started, as `ps` prints it, which tells it from an unrelated process that got its pid since.
    /// `None` when it could not be read.
    pub(crate) started: Option<String>,
    /// The port its health check probes.
    pub(crate) port: Option<u16>,
    /// The port of every Lease of the Service.
    pub(crate) lease_ports: Vec<u16>,
    /// The Services it covers when it is a Group, whose Leases it holds. Absent from a document written before
    /// Groups existed, where no Service is one.
    #[serde(default)]
    pub(crate) covers: Vec<String>,
    /// The port each Service a Group covers is probed on, which the Group must still listen on to be re-attached.
    #[serde(default)]
    pub(crate) covered_ports: Vec<u16>,
    /// How long the Service may be idle before it is stopped.
    pub(crate) idle_ms: Option<u64>,
}

impl ServiceRecord {
    pub(crate) fn topology(&self) -> Topology {
        Topology {
            git_common_dir: self.repository.clone(),
            worktree: self.worktree.clone(),
        }
    }

    fn key(&self) -> Key {
        (self.worktree.path.clone(), self.service.clone())
    }
}

/// A Service runs at most once per Worktree: it is keyed by the root of its Worktree and its name.
type Key = (PathBuf, String);

/// The document holding every record.
#[derive(Debug, Serialize, Deserialize)]
struct ServiceDocument {
    services: Vec<ServiceRecord>,
}

/// The records of the running Services, written to their document on every change.
#[derive(Debug)]
pub(crate) struct ServiceRecords {
    path: PathBuf,
    /// Held while the document is written, so that it always ends with the latest table.
    table: Mutex<BTreeMap<Key, ServiceRecord>>,
}

impl ServiceRecords {
    /// The records stored in the document at `path`, empty until [`ServiceRecords::replace`] fills them.
    pub(crate) fn new(path: PathBuf) -> Self {
        Self {
            path,
            table: Mutex::default(),
        }
    }

    /// The records a previous Daemon left in the document. Blocking.
    pub(crate) fn load(&self) -> Result<Vec<ServiceRecord>, StoreError> {
        Ok(store::load::<ServiceDocument>(&self.path, DOCUMENT_VERSION)?
            .map_or_else(Vec::new, |document| document.services))
    }

    /// Replaces every record by `records`, and the document with them. Blocking.
    pub(crate) fn replace(&self, records: Vec<ServiceRecord>) {
        let mut table = self.table();
        *table = records.into_iter().map(|record| (record.key(), record)).collect();
        self.write(&table);
    }

    /// Records the current run of a Service, replacing its previous one. Blocking.
    pub(crate) fn put(&self, record: ServiceRecord) {
        let mut table = self.table();
        if table.get(&record.key()) != Some(&record) {
            table.insert(record.key(), record);
            self.write(&table);
        }
    }

    /// Forgets the Service `service` of `worktree`, which has ended. Blocking.
    pub(crate) fn remove(&self, worktree: PathBuf, service: String) {
        let mut table = self.table();
        if table.remove(&(worktree, service)).is_some() {
            self.write(&table);
        }
    }

    /// A Service that could not be recorded keeps running, the next Daemon would only fail to re-attach it.
    fn write(&self, table: &BTreeMap<Key, ServiceRecord>) {
        let document = ServiceDocument {
            services: table.values().cloned().collect(),
        };
        if let Err(error) = store::save(&self.path, DOCUMENT_VERSION, &document) {
            let cause = std::error::Error::source(&error)
                .map(ToString::to_string)
                .unwrap_or_default();
            eprintln!("{error}: {cause}");
        }
    }

    fn table(&self) -> std::sync::MutexGuard<'_, BTreeMap<Key, ServiceRecord>> {
        self.table
            .lock()
            .expect("the service record mutex is never poisoned, no code panics while holding it")
    }
}
