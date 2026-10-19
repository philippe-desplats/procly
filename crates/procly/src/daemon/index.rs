//! Indexes: the number each Logical resource of a Driver that numbers its resources gets in each Worktree, such as the
//! database of a Redis server (ADR 0005).
//!
//! A number is handed out once and stays the same across restarts of the Daemon, so it is stored in a document of the
//! state directory (ADR 0012). The Main worktree always has the lowest numbers, whichever Worktree asks first, as it
//! keeps the declared names and ports.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use super::store::{self, StoreError};

/// The version of the format of the Index document, bumped when a field changes meaning or disappears.
const DOCUMENT_VERSION: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub(crate) enum IndexError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(
        "all {capacity} indexes of service {service} are taken, none is left for resource {resource}: raise `capacity` in \
         the Driver {driver} by copying it to .procly/drivers/{driver}.toml, together with the databases the server \
         holds, or set `isolation = \"shared\"` on the service so that every Worktree uses the same index"
    )]
    Exhausted {
        service: String,
        driver: String,
        resource: String,
        capacity: u32,
    },
}

/// What an index binds: one resource of one Compose service in one Worktree of one Repository.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct IndexKey {
    /// The common git directory identifying the Repository.
    repository: PathBuf,
    /// The root of the Worktree.
    worktree: PathBuf,
    service: String,
    /// The key of the resource in the configuration.
    resource: String,
}

type Table = BTreeMap<IndexKey, u32>;

/// One index as the document stores it.
#[derive(Debug, Serialize, Deserialize)]
struct Index {
    #[serde(flatten)]
    key: IndexKey,
    index: u32,
}

/// The document holding every index.
#[derive(Debug, Serialize, Deserialize)]
struct IndexDocument {
    indexes: Vec<Index>,
}

/// Every index the Daemon handed out, across all Repositories, so that no 2 resources of one container share one.
#[derive(Debug)]
pub(crate) struct Indexes {
    path: PathBuf,
    /// Read from the document on the first request, so that an unreadable document fails that request, naming the
    /// file, instead of keeping the Daemon from starting. `None` until then.
    table: Mutex<Option<Table>>,
}

/// What a Compose service asks indexes for.
#[derive(Debug)]
pub(crate) struct IndexRequest<'a> {
    /// The common git directory of the Repository.
    pub(crate) repository: &'a Path,
    /// The root of the Main worktree, whose resources are numbered first.
    pub(crate) main: &'a Path,
    /// The root of the Worktree whose indexes are wanted: the Worktree asking, or the Main worktree for a Service
    /// that is Shared, so that every Worktree reads the same ones.
    pub(crate) worktree: &'a Path,
    pub(crate) service: &'a str,
    /// The Driver, named in the error when no index is left.
    pub(crate) driver: &'a str,
    /// How many indexes the container holds.
    pub(crate) capacity: u32,
    /// The keys of the resources, in the order they are numbered.
    pub(crate) resources: &'a [String],
}

impl Indexes {
    /// The indexes stored in the document at `path`.
    pub(crate) fn new(path: PathBuf) -> Self {
        Self {
            path,
            table: Mutex::new(None),
        }
    }

    /// The index of each resource of the request, handing out the ones it lacks and storing them before it returns.
    /// Blocking: it reads and writes the document.
    pub(crate) fn allocate(&self, request: &IndexRequest<'_>) -> Result<BTreeMap<String, u32>, IndexError> {
        let mut table = self
            .table
            .lock()
            .expect("the index table mutex is never poisoned, no code panics while holding it");
        let table = match &mut *table {
            Some(table) => table,
            None => table.insert(self.read()?),
        };
        // Handed out on a copy, so that an index the document failed to store is not given away.
        let mut allocated_table = table.clone();
        let allocated = allocate_in(&mut allocated_table, request)?;
        if allocated_table != *table {
            self.write(&allocated_table)?;
            *table = allocated_table;
        }
        Ok(allocated)
    }

    fn read(&self) -> Result<Table, StoreError> {
        let document = store::load::<IndexDocument>(&self.path, DOCUMENT_VERSION)?;
        Ok(document
            .map(|document| {
                document
                    .indexes
                    .into_iter()
                    .map(|index| (index.key, index.index))
                    .collect()
            })
            .unwrap_or_default())
    }

    fn write(&self, table: &Table) -> Result<(), StoreError> {
        let indexes = table
            .iter()
            .map(|(key, &index)| Index {
                key: key.clone(),
                index,
            })
            .collect();
        store::save(&self.path, DOCUMENT_VERSION, &IndexDocument { indexes })
    }
}

/// Numbers the resources of `request` in `table`: the Main worktree first, so that it has the lowest numbers
/// whichever Worktree asks first, then the Worktree. A resource that has an index keeps it.
fn allocate_in(table: &mut Table, request: &IndexRequest<'_>) -> Result<BTreeMap<String, u32>, IndexError> {
    // A Shared request names the Main worktree, which is then numbered once.
    let worktrees: &[&Path] = if request.main == request.worktree {
        &[request.main]
    } else {
        &[request.main, request.worktree]
    };
    let mut wanted = BTreeMap::new();
    for &worktree in worktrees {
        wanted = BTreeMap::new();
        for resource in request.resources {
            let key = IndexKey {
                repository: request.repository.to_owned(),
                worktree: worktree.to_owned(),
                service: request.service.to_owned(),
                resource: resource.clone(),
            };
            let index = if let Some(&index) = table.get(&key) {
                index
            } else {
                let index = first_free(table, request).ok_or_else(|| IndexError::Exhausted {
                    service: request.service.to_owned(),
                    driver: request.driver.to_owned(),
                    resource: resource.clone(),
                    capacity: request.capacity,
                })?;
                table.insert(key, index);
                index
            };
            wanted.insert(resource.clone(), index);
        }
    }
    Ok(wanted)
}

/// The lowest index of the container of the Service that no resource of any Worktree holds.
fn first_free(table: &Table, request: &IndexRequest<'_>) -> Option<u32> {
    let taken: BTreeSet<u32> = table
        .iter()
        .filter(|(key, _)| key.repository == request.repository && key.service == request.service)
        .map(|(_, &index)| index)
        .collect();
    (0..request.capacity).find(|index| !taken.contains(index))
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPOSITORY: &str = "/src/app/.git";
    const MAIN: &str = "/src/app";

    fn keys(names: &[&str]) -> Vec<String> {
        names.iter().map(|&name| name.to_owned()).collect()
    }

    /// The indexes of `resources` of service `cache` in `worktree`, in a container holding `capacity`.
    fn allocate(table: &mut Table, worktree: &str, resources: &[&str], capacity: u32) -> Result<Vec<u32>, IndexError> {
        let resources = keys(resources);
        let request = IndexRequest {
            repository: Path::new(REPOSITORY),
            main: Path::new(MAIN),
            worktree: Path::new(worktree),
            service: "cache",
            driver: "redis",
            capacity,
            resources: &resources,
        };
        allocate_in(table, &request).map(|allocated| allocated.into_values().collect())
    }

    #[test]
    fn worktrees_get_distinct_indexes_that_stay_theirs() {
        let mut table = Table::new();
        assert_eq!(allocate(&mut table, MAIN, &["main"], 16).unwrap(), [0]);
        assert_eq!(allocate(&mut table, "/src/app-a", &["main"], 16).unwrap(), [1]);
        assert_eq!(allocate(&mut table, "/src/app-b", &["main"], 16).unwrap(), [2]);
        assert_eq!(allocate(&mut table, "/src/app-a", &["main"], 16).unwrap(), [1]);
        assert_eq!(allocate(&mut table, MAIN, &["main"], 16).unwrap(), [0]);
    }

    #[test]
    fn the_main_worktree_has_the_lowest_indexes_even_when_another_worktree_asks_first() {
        let mut table = Table::new();
        assert_eq!(allocate(&mut table, "/src/app-a", &["main"], 16).unwrap(), [1]);
        assert_eq!(allocate(&mut table, MAIN, &["main"], 16).unwrap(), [0]);
    }

    #[test]
    fn each_resource_of_a_service_has_an_index_of_its_own_in_the_order_of_its_keys() {
        let mut table = Table::new();
        assert_eq!(allocate(&mut table, MAIN, &["cache", "queue"], 16).unwrap(), [0, 1]);
        assert_eq!(
            allocate(&mut table, "/src/app-a", &["cache", "queue"], 16).unwrap(),
            [2, 3]
        );
        // A resource added later takes the next index in the Main worktree first, then in the Worktree, the others
        // keep theirs.
        assert_eq!(
            allocate(&mut table, "/src/app-a", &["cache", "limiter", "queue"], 16).unwrap(),
            [2, 5, 3]
        );
        assert_eq!(
            allocate(&mut table, MAIN, &["cache", "limiter", "queue"], 16).unwrap(),
            [0, 4, 1]
        );
    }

    #[test]
    fn a_container_with_no_index_left_refuses_the_next_worktree_and_stores_nothing() {
        let mut table = Table::new();
        allocate(&mut table, MAIN, &["main"], 2).unwrap();
        allocate(&mut table, "/src/app-a", &["main"], 2).unwrap();
        let stored = table.clone();

        let error = allocate(&mut table, "/src/app-b", &["main"], 2).unwrap_err();

        let message = error.to_string();
        assert!(
            message.contains("all 2 indexes of service cache are taken"),
            "{message}"
        );
        assert!(message.contains("raise `capacity` in the Driver redis"), "{message}");
        assert!(message.contains("isolation = \"shared\""), "{message}");
        assert!(matches!(error, IndexError::Exhausted { capacity: 2, .. }));
        assert_eq!(table, stored);
    }

    #[test]
    fn a_shared_service_reads_the_indexes_of_the_main_worktree_and_takes_no_other() {
        let mut table = Table::new();
        let main = keys(&["main"]);
        // Whichever Worktree asks, the request of a Shared service names the Main worktree.
        let shared = |table: &mut Table| {
            let request = IndexRequest {
                repository: Path::new(REPOSITORY),
                main: Path::new(MAIN),
                worktree: Path::new(MAIN),
                service: "cache",
                driver: "redis",
                capacity: 1,
                resources: &main,
            };
            allocate_in(table, &request).unwrap()["main"]
        };
        assert_eq!(shared(&mut table), 0);
        assert_eq!(shared(&mut table), 0);
        assert_eq!(table.len(), 1);
    }

    #[test]
    fn services_and_repositories_number_their_own_containers() {
        let mut table = Table::new();
        let resources = keys(&["main"]);
        let mut index_in = |repository: &str, service: &str| {
            let request = IndexRequest {
                repository: Path::new(repository),
                main: Path::new(MAIN),
                worktree: Path::new(MAIN),
                service,
                driver: "redis",
                capacity: 16,
                resources: &resources,
            };
            allocate_in(&mut table, &request).unwrap()["main"]
        };
        assert_eq!(index_in(REPOSITORY, "cache"), 0);
        assert_eq!(index_in(REPOSITORY, "queue"), 0);
        assert_eq!(index_in("/src/other/.git", "cache"), 0);
    }

    #[test]
    fn indexes_survive_the_daemon_that_handed_them_out() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("indexes.json");
        let resources = keys(&["main"]);
        let request = |worktree: &'static str| IndexRequest {
            repository: Path::new(REPOSITORY),
            main: Path::new(MAIN),
            worktree: Path::new(worktree),
            service: "cache",
            driver: "redis",
            capacity: 16,
            resources: &resources,
        };
        let first = Indexes::new(path.clone());
        assert_eq!(first.allocate(&request("/src/app-a")).unwrap()["main"], 1);

        let restarted = Indexes::new(path);
        assert_eq!(restarted.allocate(&request("/src/app-a")).unwrap()["main"], 1);
        assert_eq!(restarted.allocate(&request("/src/app-b")).unwrap()["main"], 2);
    }

    #[test]
    fn an_unreadable_document_fails_the_request_naming_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("indexes.json");
        std::fs::write(&path, "not json").unwrap();
        let resources = keys(&["main"]);
        let request = IndexRequest {
            repository: Path::new(REPOSITORY),
            main: Path::new(MAIN),
            worktree: Path::new(MAIN),
            service: "cache",
            driver: "redis",
            capacity: 16,
            resources: &resources,
        };
        let error = Indexes::new(path).allocate(&request).unwrap_err();
        assert!(error.to_string().contains("indexes.json"), "{error}");
    }
}
