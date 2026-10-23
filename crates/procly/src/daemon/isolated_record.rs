//! The record of every Isolated container Procly started or adopted, kept in a document of the state directory until
//! `procly gc --apply` removes it, so that the container and the volumes a removed Worktree left behind are found
//! again and never guessed from what Docker runs (ADR 0004).
//!
//! The record of a running container is in the container document as well, which only holds the ones that run. This
//! one outlives the container stopping, since a stopped container keeps its data. It keeps the Retention policy the
//! Worktree declared, and when the Daemon found the Worktree removed, so that a delayed deletion survives a restart.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::store::{self, StoreError};
use crate::config::Retention;

/// The version of the format of the document, bumped when a field changes meaning or disappears.
const DOCUMENT_VERSION: u32 = 1;

/// One Isolated container, as much as `gc` needs to find it and its volumes and to remove them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct IsolatedRecord {
    /// The common git directory identifying the Repository.
    pub(crate) repository: PathBuf,
    /// The root of the Worktree the container runs for, which compose labels it with as its working directory.
    pub(crate) worktree: PathBuf,
    /// The name of the Service in `procly.toml`.
    pub(crate) service: String,
    /// The service of the compose file the Service maps to.
    pub(crate) compose: String,
    /// The compose project of the Worktree.
    pub(crate) project: String,
    /// What `docker` needs to reach the engine, as the container record keeps it.
    pub(crate) env: Vec<(String, String)>,
    /// The Pinned branch the Worktree was on, whose container is never an Orphan.
    pub(crate) pinned: Option<String>,
    /// The volumes of a container `gc` removed but could not remove with it, so that they stay Orphans.
    #[serde(default)]
    pub(crate) volumes: Vec<String>,
    /// What the Daemon does with the container and its volumes once it finds the Worktree removed.
    #[serde(default)]
    pub(crate) retention: Retention,
    /// When the Daemon found the Worktree removed, in milliseconds since the Unix epoch on its clock, which a delayed
    /// deletion counts from. `None` while the Worktree is not known to be removed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) removed_ms: Option<u64>,
}

impl IsolatedRecord {
    pub(crate) fn key(&self) -> Key {
        (self.repository.clone(), self.service.clone(), self.worktree.clone())
    }
}

/// An Isolated container is keyed by its Repository, the name of its Service and its Worktree.
pub(crate) type Key = (PathBuf, String, PathBuf);

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct IsolatedDocument {
    containers: Vec<IsolatedRecord>,
}

impl store::Stored for IsolatedDocument {
    const VERSION: u32 = DOCUMENT_VERSION;
}

/// The records of the Isolated containers, read from their document on first use and written on every change.
#[derive(Debug)]
pub(crate) struct IsolatedRecords {
    path: PathBuf,
    /// `None` until the document is read.
    table: Mutex<Option<BTreeMap<Key, IsolatedRecord>>>,
}

impl IsolatedRecords {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self {
            path,
            table: Mutex::default(),
        }
    }

    /// Every record, in the order of their keys. Blocking.
    pub(crate) fn all(&self) -> Result<Vec<IsolatedRecord>, StoreError> {
        let mut table = self.table();
        Ok(self.loaded(&mut table)?.values().cloned().collect())
    }

    /// Records the container of `record`, keeping the volumes a previous `gc` left of the same container. Blocking.
    pub(crate) fn record(&self, mut record: IsolatedRecord) -> Result<(), StoreError> {
        let mut table = self.table();
        let loaded = self.loaded(&mut table)?;
        let key = record.key();
        if let Some(previous) = loaded.get(&key) {
            if *previous == record {
                return Ok(());
            }
            for volume in &previous.volumes {
                if !record.volumes.contains(volume) {
                    record.volumes.push(volume.clone());
                }
            }
        }
        loaded.insert(key, record);
        self.write(loaded)
    }

    /// Records whether the Worktree of the container `key` is removed, keeping the moment the Daemon first found it
    /// removed, and returns that moment, `None` once the Worktree is not removed, or the container is not recorded.
    /// Blocking.
    pub(crate) fn found_removed(
        &self,
        key: &Key,
        removed: bool,
        now: Duration,
    ) -> Result<Option<Duration>, StoreError> {
        let mut table = self.table();
        let loaded = self.loaded(&mut table)?;
        let Some(record) = loaded.get_mut(key) else {
            return Ok(None);
        };
        let marked = match (removed, record.removed_ms) {
            (true, Some(found)) => return Ok(Some(Duration::from_millis(found))),
            (false, None) => return Ok(None),
            (true, None) => Some(u64::try_from(now.as_millis()).unwrap_or(u64::MAX)),
            (false, Some(_)) => None,
        };
        record.removed_ms = marked;
        self.write(loaded)?;
        Ok(marked.map(Duration::from_millis))
    }

    /// Sets the Retention policy of the container `key` to `retention`, as its Worktree declares it now. Blocking.
    pub(crate) fn retain(&self, key: &Key, retention: Retention) -> Result<(), StoreError> {
        let mut table = self.table();
        let loaded = self.loaded(&mut table)?;
        match loaded.get_mut(key) {
            Some(record) if record.retention != retention => record.retention = retention,
            _ => return Ok(()),
        }
        self.write(loaded)
    }

    /// Keeps `volumes` as what is left of the container `key` once it is removed, or forgets the container when
    /// none is left. Blocking.
    pub(crate) fn removed(&self, key: &Key, volumes: Vec<String>) -> Result<(), StoreError> {
        let mut table = self.table();
        let loaded = self.loaded(&mut table)?;
        if volumes.is_empty() {
            if loaded.remove(key).is_none() {
                return Ok(());
            }
        } else if let Some(record) = loaded.get_mut(key) {
            if record.volumes == volumes {
                return Ok(());
            }
            record.volumes = volumes;
        } else {
            return Ok(());
        }
        self.write(loaded)
    }

    fn loaded<'a>(
        &self,
        table: &'a mut Option<BTreeMap<Key, IsolatedRecord>>,
    ) -> Result<&'a mut BTreeMap<Key, IsolatedRecord>, StoreError> {
        if let Some(loaded) = table {
            return Ok(loaded);
        }
        let records = store::load::<IsolatedDocument>(&self.path, DOCUMENT_VERSION)?
            .map_or_else(Vec::new, |document| document.containers);
        Ok(table.insert(records.into_iter().map(|record| (record.key(), record)).collect()))
    }

    fn write(&self, table: &BTreeMap<Key, IsolatedRecord>) -> Result<(), StoreError> {
        let document = IsolatedDocument {
            containers: table.values().cloned().collect(),
        };
        store::save(&self.path, DOCUMENT_VERSION, &document)
    }

    fn table(&self) -> MutexGuard<'_, Option<BTreeMap<Key, IsolatedRecord>>> {
        self.table
            .lock()
            .expect("the isolated record mutex is never poisoned, no code panics while holding it")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(volumes: &[&str]) -> IsolatedRecord {
        IsolatedRecord {
            repository: PathBuf::from("/src/app/.git"),
            worktree: PathBuf::from("/src/app-login"),
            service: "db".to_owned(),
            compose: "db".to_owned(),
            project: "app-app_login".to_owned(),
            env: Vec::new(),
            pinned: None,
            volumes: volumes.iter().map(|&volume| volume.to_owned()).collect(),
            retention: Retention::After(Duration::from_hours(24)),
            removed_ms: None,
        }
    }

    #[test]
    fn a_record_outlives_the_daemon_and_keeps_the_volumes_left_of_its_container() {
        let dir = tempfile::tempdir().expect("create a temporary directory");
        let path = dir.path().join("isolated.json");
        let records = IsolatedRecords::new(path.clone());
        records.record(record(&[])).expect("record");
        records
            .removed(&record(&[]).key(), vec!["app-app_login_data".to_owned()])
            .expect("keep the volume");
        // Started again before the next gc, it keeps the volume to remove.
        records.record(record(&[])).expect("record again");

        let reread = IsolatedRecords::new(path);

        assert_eq!(reread.all().expect("read"), [record(&["app-app_login_data"])]);
        assert_eq!(
            reread
                .found_removed(&record(&[]).key(), true, Duration::from_secs(5))
                .expect("mark"),
            Some(Duration::from_secs(5))
        );
        // The moment the Worktree was first found removed is kept, and outlives the Daemon.
        let reread = IsolatedRecords::new(dir.path().join("isolated.json"));
        assert_eq!(
            reread
                .found_removed(&record(&[]).key(), true, Duration::from_secs(9))
                .expect("mark again"),
            Some(Duration::from_secs(5))
        );
        assert_eq!(
            reread.all().expect("read")[0].retention,
            Retention::After(Duration::from_hours(24))
        );
        assert_eq!(
            reread
                .found_removed(&record(&[]).key(), false, Duration::from_secs(9))
                .expect("unmark"),
            None
        );
        reread.removed(&record(&[]).key(), Vec::new()).expect("forget");
        assert_eq!(reread.all().expect("read"), []);
    }
}
