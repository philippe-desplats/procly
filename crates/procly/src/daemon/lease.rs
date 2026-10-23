//! Leases: the port each Port name of each Service gets in each Worktree, or for each Pinned branch (ADR 0006).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::net::TcpSocket;

use super::clock::Clock;
use super::store::{self, StoreError};
use crate::config::PortConfig;
use crate::ports::Range;
use crate::protocol::{KeptBecause, KeptLease, LeaseHeld};
use crate::topology::Topology;

/// The version of the format of the Lease document, bumped when a field changes meaning or disappears.
///
/// Version 2 binds the Lease of a Pinned branch to `branch` instead of `worktree`, and records `last_used_ms`.
const DOCUMENT_VERSION: u32 = 2;

#[derive(Debug, thiserror::Error)]
pub(crate) enum LeaseError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(
        "no port of the Range {range} is free for Port name {port_name} of service {service}, widen settings.port_range"
    )]
    RangeExhausted {
        service: String,
        port_name: String,
        range: Range,
    },
}

/// What a Lease is bound to within its Repository: the Worktree, or the branch for a Pinned branch, so that the
/// Lease survives the Worktree being removed and recreated.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Scope {
    /// The root of the Worktree, for a branch that is not pinned, so that 2 Worktrees never share a Lease.
    Worktree(PathBuf),
    /// The name of the Pinned branch, so that whichever Worktree checks it out gets its Leases.
    Branch(String),
}

/// What a Lease binds: one Port name of one Service in one Worktree, or one Pinned branch, of one Repository.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(super) struct LeaseKey {
    /// The common git directory identifying the Repository.
    pub(super) repository: PathBuf,
    #[serde(flatten)]
    pub(super) scope: Scope,
    pub(super) service: String,
    pub(super) port_name: String,
}

/// A granted port, and when a Service last used it, read on [`Clock::since_epoch`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Granted {
    port: u16,
    /// Compared with `settings.lease_expiry` to drop a Lease no Service used for that long.
    last_used: Duration,
}

type Table = BTreeMap<LeaseKey, Granted>;

/// One Lease as the document stores it.
#[derive(Debug, Serialize, Deserialize)]
struct Lease {
    #[serde(flatten)]
    key: LeaseKey,
    port: u16,
    /// Milliseconds since the Unix epoch, absent from a document of version 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_used_ms: Option<u64>,
}

/// The document holding every Lease.
#[derive(Debug, Serialize, Deserialize)]
struct LeaseDocument {
    leases: Vec<Lease>,
}

/// Whether the document at `path` can be read, as the next request that needs it reads it. Nothing is kept or written.
pub(super) fn readable(path: &Path) -> Result<(), StoreError> {
    store::load::<LeaseDocument>(path, DOCUMENT_VERSION).map(|_| ())
}

/// A Lease bound to a Worktree rather than to a Pinned branch, as `procly gc` lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorktreeLease {
    /// The common git directory identifying the Repository.
    pub(crate) repository: PathBuf,
    /// The root of the Worktree.
    pub(crate) worktree: PathBuf,
    pub(crate) service: String,
    pub(crate) port_name: String,
    pub(crate) port: u16,
}

/// Every Lease the Daemon granted, across all Repositories, so that no 2 of them share a port. They persist in a
/// document of the state directory, so they survive restarts of the Daemon.
#[derive(Debug)]
pub(crate) struct Leases {
    path: PathBuf,
    clock: Clock,
    /// Read from the document on the first request, so that an unreadable document fails that request, naming the
    /// file, instead of keeping the Daemon from starting. `None` until then.
    table: Mutex<Option<Table>>,
}

/// What a Service asks Leases for.
#[derive(Debug)]
pub(crate) struct LeaseRequest<'a> {
    pub(crate) topology: &'a Topology,
    pub(crate) service: &'a str,
    /// The declared port and the Pool of each Port name of the Service.
    pub(crate) ports: &'a BTreeMap<String, PortConfig>,
    pub(crate) range: Range,
    /// Every port the configuration declares, which the Range skips.
    pub(crate) declared: &'a BTreeSet<u16>,
    /// The Pinned branch the Worktree is on, `None` when its branch is not pinned.
    pub(crate) pinned: Option<PinnedBranch<'a>>,
    /// How long a Lease no Service used is kept, Pinned branches aside.
    pub(crate) expiry: Duration,
    /// The Worktree root and the name of every running Service, whose Leases are in use and never expire.
    pub(crate) running: &'a BTreeSet<(PathBuf, String)>,
    /// Keeps a Lease already granted as it is, even where a freed preferred port would take it back. For a request
    /// that does not start the Service, as the Service may run on that Lease right now.
    pub(crate) keep: bool,
    /// For a Compose service, whether a preferred port that is not free is held by its own container started outside
    /// Procly, so that the Lease is the port Adoption then finds the container on.
    pub(crate) claimed: Option<Claimed<'a>>,
}

/// Tells whether the container of the Service itself holds a port. Asked about the preferred port only, since each
/// question runs `docker`.
#[derive(Clone, Copy)]
pub(crate) struct Claimed<'a>(pub(crate) &'a dyn Fn(u16) -> bool);

impl fmt::Debug for Claimed<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Claimed(..)")
    }
}

/// The Pinned branch a Worktree is on.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PinnedBranch<'a> {
    pub(crate) name: &'a str,
    /// The explicit port of some Port names of the Service.
    pub(crate) ports: Option<&'a BTreeMap<String, u16>>,
}

impl Leases {
    /// The Leases stored in the document at `path`, whose use is dated on `clock`.
    pub(crate) fn new(path: PathBuf, clock: Clock) -> Self {
        Self {
            path,
            clock,
            table: Mutex::new(None),
        }
    }

    /// The leased port of each Port name of the Service, granting the Leases it lacks and storing them before it
    /// returns. Leases of the Repository that expired are dropped first, so that their ports return to the Range.
    /// Each grant dates the Leases it hands out, so the document is rewritten on every start of a Service. Blocking: it reads and writes the document, and binds the candidate ports to check that nothing else holds
    /// them.
    pub(crate) fn grant(&self, request: &LeaseRequest<'_>) -> Result<BTreeMap<String, u16>, LeaseError> {
        let now = self.clock.since_epoch();
        let mut table = self
            .table
            .lock()
            .expect("the lease table mutex is never poisoned, no code panics while holding it");
        let table = match &mut *table {
            Some(table) => table,
            None => table.insert(self.read(now)?),
        };
        // Granted on a copy, so that a Lease the document failed to store is not handed out.
        let mut granted_table = table.clone();
        expire(&mut granted_table, request, now);
        let granted = grant_in(&mut granted_table, request, now, is_free)?;
        if granted_table != *table {
            self.write(&granted_table)?;
            *table = granted_table;
        }
        Ok(granted)
    }

    /// Dates the Leases of the Service `service` of `worktree` as used now, for a Service that just ended. Leases of
    /// a Pinned branch are left alone, they never expire. Blocking: it writes the document.
    pub(crate) fn touch(&self, worktree: &Path, service: &str) -> Result<(), StoreError> {
        let now = self.clock.since_epoch();
        let mut table = self
            .table
            .lock()
            .expect("the lease table mutex is never poisoned, no code panics while holding it");
        // Read on the grant that started the Service, so only a failed read leaves nothing to date.
        let Some(table) = &mut *table else {
            return Ok(());
        };
        let mut touched = table.clone();
        for (key, granted) in &mut touched {
            if matches!(&key.scope, Scope::Worktree(path) if path == worktree) && key.service == service {
                granted.last_used = now;
            }
        }
        if touched != *table {
            self.write(&touched)?;
            *table = touched;
        }
        Ok(())
    }

    /// Every Lease bound to a Worktree, those of Pinned branches left out, in the order of the document. Blocking: it
    /// reads the document on the first request.
    pub(crate) fn bound_to_worktrees(&self) -> Result<Vec<WorktreeLease>, StoreError> {
        let mut table = self
            .table
            .lock()
            .expect("the lease table mutex is never poisoned, no code panics while holding it");
        let table = match &mut *table {
            Some(table) => table,
            None => table.insert(self.read(self.clock.since_epoch())?),
        };
        Ok(table
            .iter()
            .filter_map(|(key, granted)| match &key.scope {
                Scope::Worktree(worktree) => Some(WorktreeLease {
                    repository: key.repository.clone(),
                    worktree: worktree.clone(),
                    service: key.service.clone(),
                    port_name: key.port_name.clone(),
                    port: granted.port,
                }),
                Scope::Branch(_) => None,
            })
            .collect())
    }

    /// What binds the Lease on `port` in the Repository `repository`, `None` when no Lease holds it. A port is held by
    /// one Lease at most. Blocking: it reads the document on the first request.
    pub(super) fn bound_to(&self, repository: &Path, port: u16) -> Result<Option<LeaseKey>, StoreError> {
        self.with_table(|table| {
            table
                .iter()
                .find(|(key, granted)| key.repository == repository && granted.port == port)
                .map(|(key, _)| key.clone())
        })
    }

    /// Whether the Lease `key` is granted `port` right now. Blocking: it reads the document on the first request.
    pub(super) fn holds(&self, key: &LeaseKey, port: u16) -> Result<bool, StoreError> {
        self.with_table(|table| table.get(key).map(|granted| granted.port) == Some(port))
    }

    /// Runs `read` on the Leases, reading the document first if no request did.
    fn with_table<T>(&self, read: impl FnOnce(&Table) -> T) -> Result<T, StoreError> {
        let mut table = self
            .table
            .lock()
            .expect("the lease table mutex is never poisoned, no code panics while holding it");
        let table = match &mut *table {
            Some(table) => table,
            None => table.insert(self.read(self.clock.since_epoch())?),
        };
        Ok(read(table))
    }

    /// Releases `lease`, so that its port returns to the Range, and stores the Leases left before it returns. A Lease
    /// granted another port since it was read is left as it is. Blocking: it writes the document.
    pub(crate) fn release(&self, lease: &WorktreeLease) -> Result<(), StoreError> {
        let mut table = self
            .table
            .lock()
            .expect("the lease table mutex is never poisoned, no code panics while holding it");
        let table = match &mut *table {
            Some(table) => table,
            None => table.insert(self.read(self.clock.since_epoch())?),
        };
        let key = LeaseKey {
            repository: lease.repository.clone(),
            scope: Scope::Worktree(lease.worktree.clone()),
            service: lease.service.clone(),
            port_name: lease.port_name.clone(),
        };
        if table.get(&key).map(|granted| granted.port) != Some(lease.port) {
            return Ok(());
        }
        let mut released = table.clone();
        released.remove(&key);
        self.write(&released)?;
        *table = released;
        Ok(())
    }

    /// Releases the Leases of `worktree` in the Repository `repository`, so that any Worktree can be granted their
    /// ports at once, and returns them with the Leases it kept: those of the Pinned branch `branch` the Worktree is
    /// on, and those a Service of `running` still uses, such as a container that serves other Worktrees. Blocking:
    /// it reads and writes the document.
    pub(crate) fn release_worktree(
        &self,
        repository: &Path,
        worktree: &Path,
        branch: Option<&str>,
        running: &BTreeSet<(PathBuf, String)>,
    ) -> Result<(Vec<LeaseHeld>, Vec<KeptLease>), StoreError> {
        let now = self.clock.since_epoch();
        let mut table = self
            .table
            .lock()
            .expect("the lease table mutex is never poisoned, no code panics while holding it");
        let table = match &mut *table {
            Some(table) => table,
            None => table.insert(self.read(now)?),
        };
        // Released on a copy, so that a Lease the document still holds is not reported as released.
        let mut released_table = table.clone();
        let released = release_in(&mut released_table, repository, worktree, branch, running);
        if released_table != *table {
            self.write(&released_table)?;
            *table = released_table;
        }
        Ok(released)
    }

    /// Reads the document, a Lease of a document of version 1 counting as used at `now`.
    fn read(&self, now: Duration) -> Result<Table, StoreError> {
        let document = store::load::<LeaseDocument>(&self.path, DOCUMENT_VERSION)?;
        Ok(document
            .map(|document| {
                document
                    .leases
                    .into_iter()
                    .map(|lease| {
                        let last_used = lease.last_used_ms.map_or(now, Duration::from_millis);
                        (
                            lease.key,
                            Granted {
                                port: lease.port,
                                last_used,
                            },
                        )
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    fn write(&self, table: &Table) -> Result<(), StoreError> {
        let leases = table
            .iter()
            .map(|(key, granted)| Lease {
                key: key.clone(),
                port: granted.port,
                // Saturated, a reading past year 500 million is no date anyway.
                last_used_ms: Some(u64::try_from(granted.last_used.as_millis()).unwrap_or(u64::MAX)),
            })
            .collect();
        store::save(&self.path, DOCUMENT_VERSION, &LeaseDocument { leases })
    }
}

/// Drops the Leases of the Repository of `request` that no Service used for `request.expiry`, after marking the
/// Leases of running Services as used at `now`. A Lease of a Pinned branch never expires.
///
/// Leases of other Repositories are left to their own settings, they expire on the next request from them.
fn expire(table: &mut Table, request: &LeaseRequest<'_>, now: Duration) {
    let repository = &request.topology.git_common_dir;
    for (key, granted) in table.iter_mut() {
        if let Scope::Worktree(worktree) = &key.scope
            && key.repository == *repository
            && request.running.contains(&(worktree.clone(), key.service.clone()))
        {
            granted.last_used = now;
        }
    }
    table.retain(|key, granted| {
        key.repository != *repository
            || matches!(key.scope, Scope::Branch(_))
            || now.saturating_sub(granted.last_used) < request.expiry
    });
}

/// Drops the Leases of `worktree` in the Repository `repository` from `table`, apart from those a Service of `running`
/// uses, and returns them, then the Leases kept: those still used, and those of the Pinned branch `branch`.
fn release_in(
    table: &mut Table,
    repository: &Path,
    worktree: &Path,
    branch: Option<&str>,
    running: &BTreeSet<(PathBuf, String)>,
) -> (Vec<LeaseHeld>, Vec<KeptLease>) {
    let held = |key: &LeaseKey, granted: &Granted| LeaseHeld {
        service: key.service.clone(),
        port_name: key.port_name.clone(),
        port: granted.port,
    };
    let (mut released, mut kept) = (Vec::new(), Vec::new());
    table.retain(|key, granted| {
        if key.repository != repository {
            return true;
        }
        match &key.scope {
            Scope::Worktree(path) if path == worktree => {
                if running.contains(&(path.clone(), key.service.clone())) {
                    kept.push(KeptLease {
                        lease: held(key, granted),
                        reason: KeptBecause::ContainerRunning,
                    });
                    return true;
                }
                released.push(held(key, granted));
                false
            }
            Scope::Branch(pinned) if Some(pinned.as_str()) == branch => {
                kept.push(KeptLease {
                    lease: held(key, granted),
                    reason: KeptBecause::PinnedBranch { branch: pinned.clone() },
                });
                true
            }
            Scope::Worktree(_) | Scope::Branch(_) => true,
        }
    });
    (released, kept)
}

/// Grants the Leases of `request` in `table`, dated `now`, where `is_free` tells whether nothing outside Procly
/// holds a port.
///
/// A Lease already granted is kept as it is, so the port stays the same across restarts. The preferred port, the
/// explicit port of a Pinned branch or else the declared port in the Main worktree, is taken whenever no other
/// Lease and nothing else holds it, even when the Lease held another one before, so that editing it takes effect,
/// unless the request asks to keep its Lease. Without a Lease nor a free preferred port, the Pool is tried in order,
/// then the Range.
fn grant_in(
    table: &mut Table,
    request: &LeaseRequest<'_>,
    now: Duration,
    is_free: impl Fn(u16) -> bool,
) -> Result<BTreeMap<String, u16>, LeaseError> {
    let scope = match request.pinned {
        Some(pinned) => Scope::Branch(pinned.name.to_owned()),
        None => Scope::Worktree(request.topology.worktree.path.clone()),
    };
    let mut granted = BTreeMap::new();
    for (port_name, declared) in request.ports {
        let key = LeaseKey {
            repository: request.topology.git_common_dir.clone(),
            scope: scope.clone(),
            service: request.service.to_owned(),
            port_name: port_name.clone(),
        };
        let current = table.get(&key).map(|granted| granted.port);
        let held_elsewhere = |port: u16| {
            table
                .iter()
                .any(|(other, granted)| granted.port == port && *other != key)
        };
        let available = |port: u16| !held_elsewhere(port) && is_free(port);
        let claimed = |port: u16| !held_elsewhere(port) && request.claimed.is_some_and(|claimed| (claimed.0)(port));
        let explicit = request
            .pinned
            .and_then(|pinned| pinned.ports)
            .and_then(|ports| ports.get(port_name).copied());
        let preferred = explicit.or_else(|| request.topology.worktree.main.then_some(declared.port));
        let port = if let Some(preferred) = preferred.filter(|&port| {
            !(request.keep && current.is_some()) && current != Some(port) && (available(port) || claimed(port))
        }) {
            preferred
        } else if let Some(current) = current {
            current
        } else if let Some(pooled) = declared.pool.iter().copied().find(|&port| available(port)) {
            pooled
        } else {
            request
                .range
                .ports()
                .find(|&port| !request.declared.contains(&port) && available(port))
                .ok_or_else(|| LeaseError::RangeExhausted {
                    service: request.service.to_owned(),
                    port_name: port_name.clone(),
                    range: request.range,
                })?
        };
        table.insert(key, Granted { port, last_used: now });
        granted.insert(port_name.clone(), port);
    }
    Ok(granted)
}

/// Whether no socket holds `port` on any address, IPv4 or IPv6.
///
/// The probe binds the wildcard addresses without `SO_REUSEADDR`, which the standard library sets: with it, macOS
/// lets a bind to one address succeed while a Foreign process listens on the wildcard, or the other way round.
fn is_free(port: u16) -> bool {
    let v4 = TcpSocket::new_v4().and_then(|socket| socket.bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, port))));
    if v4.is_err() {
        return false;
    }
    match TcpSocket::new_v6().and_then(|socket| socket.bind(SocketAddr::from((Ipv6Addr::UNSPECIFIED, port)))) {
        Ok(()) => true,
        // A host without IPv6 has nothing listening on it.
        Err(error) => {
            matches!(
                error.raw_os_error(),
                Some(nix::libc::EAFNOSUPPORT | nix::libc::EADDRNOTAVAIL)
            ) || error.kind() == io::ErrorKind::Unsupported
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::topology::Worktree;

    fn topology(worktree: &str, main: bool) -> Topology {
        Topology {
            git_common_dir: PathBuf::from("/src/app/.git"),
            worktree: Worktree {
                path: PathBuf::from(worktree),
                slug: worktree.trim_start_matches('/').replace('/', "-"),
                main,
            },
        }
    }

    const RANGE: Range = Range {
        start: 24000,
        end: 24009,
    };

    /// The unit of `settings.lease_expiry` in the defaults.
    const DAY: Duration = Duration::from_hours(24);

    /// A request of `web` in `topology`, for the declared ports `ports`, expiring after 30 days.
    fn request<'a>(
        topology: &'a Topology,
        ports: &'a BTreeMap<String, PortConfig>,
        declared: &'a BTreeSet<u16>,
        running: &'a BTreeSet<(PathBuf, String)>,
    ) -> LeaseRequest<'a> {
        LeaseRequest {
            topology,
            service: "web",
            ports,
            range: RANGE,
            declared,
            pinned: None,
            expiry: 30 * DAY,
            running,
            keep: false,
            claimed: None,
        }
    }

    fn port_configs(ports: &[(&str, u16)]) -> BTreeMap<String, PortConfig> {
        ports
            .iter()
            .map(|&(name, port)| (name.to_owned(), PortConfig::declared(port)))
            .collect()
    }

    fn grant(table: &mut Table, topology: &Topology, ports: &[(&str, u16)], is_free: impl Fn(u16) -> bool) -> Vec<u16> {
        let ports = port_configs(ports);
        let declared = ports.values().map(|port| port.port).collect();
        let running = BTreeSet::new();
        let request = request(topology, &ports, &declared, &running);
        grant_in(table, &request, Duration::ZERO, is_free)
            .unwrap()
            .into_values()
            .collect()
    }

    #[test]
    fn the_main_worktree_gets_the_declared_port_and_another_worktree_the_first_of_the_range() {
        let mut table = Table::new();
        assert_eq!(
            grant(&mut table, &topology("/src/app", true), &[("http", 3000)], |_| true),
            [3000]
        );
        assert_eq!(
            grant(&mut table, &topology("/src/app-a", false), &[("http", 3000)], |_| true),
            [24000]
        );
        assert_eq!(
            grant(&mut table, &topology("/src/app-b", false), &[("http", 3000)], |_| true),
            [24001]
        );
    }

    #[test]
    fn a_lease_is_kept_even_once_its_port_is_busy() {
        let mut table = Table::new();
        let other = topology("/src/app-a", false);
        assert_eq!(grant(&mut table, &other, &[("http", 3000)], |_| true), [24000]);
        // The Service itself holds its port while it runs.
        assert_eq!(
            grant(&mut table, &other, &[("http", 3000)], |port| port != 24000),
            [24000]
        );
    }

    #[test]
    fn a_busy_port_is_skipped_and_the_main_worktree_falls_back_to_the_range() {
        let mut table = Table::new();
        let busy = |port| port != 3000 && port != 24000;
        assert_eq!(
            grant(&mut table, &topology("/src/app", true), &[("http", 3000)], busy),
            [24001]
        );
    }

    #[test]
    fn the_main_worktree_gets_its_declared_port_back_once_it_is_free() {
        let mut table = Table::new();
        let main = topology("/src/app", true);
        assert_eq!(
            grant(&mut table, &main, &[("http", 3000)], |port| port != 3000),
            [24000]
        );
        assert_eq!(grant(&mut table, &main, &[("http", 3000)], |_| true), [3000]);
        assert_eq!(grant(&mut table, &main, &[("http", 4000)], |_| true), [4000]);
    }

    #[test]
    fn a_declared_port_the_container_of_the_service_holds_is_its_lease_and_no_other_port_is_asked_about() {
        let mut table = Table::new();
        let main = topology("/src/app", true);
        let ports = port_configs(&[("db", 5432)]);
        let (declared, running) = (BTreeSet::from([5432]), BTreeSet::new());
        let asked = std::cell::RefCell::new(Vec::new());
        let holds = |port: u16| {
            asked.borrow_mut().push(port);
            port == 5432
        };
        let request = LeaseRequest {
            claimed: Some(Claimed(&holds)),
            ..request(&main, &ports, &declared, &running)
        };
        assert_eq!(
            grant_in(&mut table, &request, Duration::ZERO, |_| false).unwrap()["db"],
            5432
        );
        assert_eq!(*asked.borrow(), [5432]);
    }

    #[test]
    fn a_lease_asked_to_be_kept_stays_even_when_the_declared_port_is_free_again() {
        let mut table = Table::new();
        let main = topology("/src/app", true);
        assert_eq!(
            grant(&mut table, &main, &[("http", 3000)], |port| port != 3000),
            [24000]
        );
        let ports = port_configs(&[("http", 3000)]);
        let (declared, running) = (BTreeSet::from([3000]), BTreeSet::new());
        let request = LeaseRequest {
            keep: true,
            ..request(&main, &ports, &declared, &running)
        };
        // A Service that runs on its Lease must not see it move under a request that only reads it.
        assert_eq!(
            grant_in(&mut table, &request, Duration::ZERO, |_| true).unwrap()["http"],
            24000
        );
        // A Lease that does not exist yet is granted the usual way.
        let missing = LeaseRequest {
            service: "api",
            ..request
        };
        assert_eq!(
            grant_in(&mut table, &missing, Duration::ZERO, |_| true).unwrap()["http"],
            3000
        );
    }

    #[test]
    fn each_port_name_gets_its_own_lease_and_the_range_skips_declared_ports() {
        let mut table = Table::new();
        let ports = [("http", 24000), ("test", 24001)];
        assert_eq!(
            grant(&mut table, &topology("/src/app-a", false), &ports, |_| true),
            [24002, 24003]
        );
        assert_eq!(
            grant(&mut table, &topology("/src/app", true), &ports, |_| true),
            [24000, 24001]
        );
    }

    #[test]
    fn a_full_range_is_an_error() {
        let mut table = Table::new();
        let ports = port_configs(&[("http", 3000)]);
        let (topology, declared, running) = (topology("/src/app-a", false), BTreeSet::new(), BTreeSet::new());
        let request = request(&topology, &ports, &declared, &running);
        assert!(matches!(
            grant_in(&mut table, &request, Duration::ZERO, |_| false),
            Err(LeaseError::RangeExhausted { port_name, .. }) if port_name == "http"
        ));
    }

    #[test]
    fn the_pool_comes_before_the_range_outside_the_main_worktree() {
        let mut table = Table::new();
        let ports = BTreeMap::from([(
            "http".to_owned(),
            PortConfig {
                port: 3000,
                pool: vec![3100, 3101],
            },
        )]);
        let declared = BTreeSet::from([3000, 3100, 3101]);
        let running = BTreeSet::new();
        let mut grant = |topology: &Topology, is_free: &dyn Fn(u16) -> bool| {
            grant_in(
                &mut table,
                &request(topology, &ports, &declared, &running),
                Duration::ZERO,
                is_free,
            )
            .unwrap()["http"]
        };
        assert_eq!(grant(&topology("/src/app", true), &|_| true), 3000);
        assert_eq!(grant(&topology("/src/app-a", false), &|port| port != 3100), 3101);
        assert_eq!(grant(&topology("/src/app-b", false), &|_| true), 3100);
        assert_eq!(grant(&topology("/src/app-c", false), &|_| true), 24000);
    }

    #[test]
    fn a_pinned_branch_binds_its_lease_to_the_branch_and_prefers_its_explicit_port() {
        let mut table = Table::new();
        let ports = port_configs(&[("http", 3000), ("test", 3001)]);
        let declared = BTreeSet::from([3000, 3001, 3200]);
        let running = BTreeSet::new();
        let explicit = BTreeMap::from([("http".to_owned(), 3200)]);
        let mut grant = |worktree: &str| {
            let topology = topology(worktree, false);
            let request = LeaseRequest {
                pinned: Some(PinnedBranch {
                    name: "develop",
                    ports: Some(&explicit),
                }),
                ..request(&topology, &ports, &declared, &running)
            };
            grant_in(&mut table, &request, Duration::ZERO, |_| true).unwrap()
        };
        let first = grant("/src/app-develop");
        assert_eq!(
            first,
            BTreeMap::from([("http".to_owned(), 3200), ("test".to_owned(), 24000)])
        );
        assert_eq!(grant("/src/app-develop-again"), first);
        assert!(table.keys().all(|key| key.scope == Scope::Branch("develop".to_owned())));
    }

    #[test]
    fn a_lease_asked_to_be_kept_stays_even_when_the_explicit_port_of_its_pinned_branch_is_free_again() {
        let mut table = Table::new();
        let ports = port_configs(&[("http", 3000)]);
        let declared = BTreeSet::from([3000, 3200]);
        let running = BTreeSet::new();
        let explicit = BTreeMap::from([("http".to_owned(), 3200)]);
        let topology = topology("/src/app-develop", false);
        let request = LeaseRequest {
            pinned: Some(PinnedBranch {
                name: "develop",
                ports: Some(&explicit),
            }),
            ..request(&topology, &ports, &declared, &running)
        };
        assert_eq!(
            grant_in(&mut table, &request, Duration::ZERO, |port| port != 3200).unwrap()["http"],
            24000
        );
        let kept = LeaseRequest { keep: true, ..request };
        assert_eq!(
            grant_in(&mut table, &kept, Duration::ZERO, |_| true).unwrap()["http"],
            24000
        );
        assert_eq!(
            grant_in(&mut table, &request, Duration::ZERO, |_| true).unwrap()["http"],
            3200
        );
    }

    #[test]
    fn an_unused_lease_expires_while_a_running_or_pinned_one_does_not() {
        let lease = |scope: Scope, service: &str| LeaseKey {
            repository: PathBuf::from("/src/app/.git"),
            scope,
            service: service.to_owned(),
            port_name: "http".to_owned(),
        };
        let unused = lease(Scope::Worktree("/src/app-a".into()), "web");
        let running = lease(Scope::Worktree("/src/app-a".into()), "api");
        let pinned = lease(Scope::Branch("develop".to_owned()), "web");
        let elsewhere = LeaseKey {
            repository: PathBuf::from("/src/other/.git"),
            ..unused.clone()
        };
        let mut table: Table = [&unused, &running, &pinned, &elsewhere]
            .into_iter()
            .zip(24000..)
            .map(|(key, port)| {
                (
                    key.clone(),
                    Granted {
                        port,
                        last_used: Duration::ZERO,
                    },
                )
            })
            .collect();
        let (topology, ports, declared) = (topology("/src/app", true), BTreeMap::new(), BTreeSet::new());
        let in_use = BTreeSet::from([(PathBuf::from("/src/app-a"), "api".to_owned())]);
        let request = request(&topology, &ports, &declared, &in_use);

        expire(&mut table, &request, 29 * DAY);
        assert_eq!(table.len(), 4);
        expire(&mut table, &request, 30 * DAY);

        assert_eq!(
            table.keys().cloned().collect::<BTreeSet<_>>(),
            BTreeSet::from([running.clone(), pinned, elsewhere])
        );
        assert_eq!(table[&running].last_used, 30 * DAY);
    }

    #[test]
    fn a_release_drops_the_leases_of_the_worktree_and_keeps_those_of_its_pinned_branch_and_of_running_containers() {
        let lease = |scope: Scope, service: &str| LeaseKey {
            repository: PathBuf::from("/src/app/.git"),
            scope,
            service: service.to_owned(),
            port_name: "http".to_owned(),
        };
        let released = lease(Scope::Worktree("/src/app-a".into()), "web");
        let container = lease(Scope::Worktree("/src/app-a".into()), "db");
        let pinned = lease(Scope::Branch("develop".to_owned()), "web");
        let other_branch = lease(Scope::Branch("staging".to_owned()), "web");
        let other_worktree = lease(Scope::Worktree("/src/app-b".into()), "web");
        let elsewhere = LeaseKey {
            repository: PathBuf::from("/src/other/.git"),
            ..released.clone()
        };
        let mut table: Table = [
            &released,
            &container,
            &pinned,
            &other_branch,
            &other_worktree,
            &elsewhere,
        ]
        .into_iter()
        .zip(24000..)
        .map(|(key, port)| {
            (
                key.clone(),
                Granted {
                    port,
                    last_used: Duration::ZERO,
                },
            )
        })
        .collect();
        let running = BTreeSet::from([(PathBuf::from("/src/app-a"), "db".to_owned())]);

        let (dropped, kept) = release_in(
            &mut table,
            Path::new("/src/app/.git"),
            Path::new("/src/app-a"),
            Some("develop"),
            &running,
        );

        let held = |service: &str, port| LeaseHeld {
            service: service.to_owned(),
            port_name: "http".to_owned(),
            port,
        };
        assert_eq!(dropped, [held("web", 24000)]);
        assert_eq!(
            kept,
            [
                KeptLease {
                    lease: held("db", 24001),
                    reason: KeptBecause::ContainerRunning,
                },
                KeptLease {
                    lease: held("web", 24002),
                    reason: KeptBecause::PinnedBranch {
                        branch: "develop".to_owned()
                    },
                },
            ]
        );
        assert_eq!(
            table.keys().cloned().collect::<BTreeSet<_>>(),
            BTreeSet::from([container, pinned, other_branch, other_worktree, elsewhere])
        );
        assert_eq!(
            release_in(
                &mut table,
                Path::new("/src/app/.git"),
                Path::new("/src/app-a"),
                None,
                &BTreeSet::new()
            )
            .0,
            [held("db", 24001)]
        );
    }

    #[test]
    fn a_lease_of_a_document_of_version_1_reads_as_bound_to_its_worktree() {
        let lease: Lease = serde_json::from_str(
            r#"{"repository": "/src/app/.git", "worktree": "/src/app-a", "service": "web", "port_name": "http", "port": 24000}"#,
        )
        .unwrap();
        assert_eq!(lease.key.scope, Scope::Worktree("/src/app-a".into()));
        assert_eq!((lease.port, lease.last_used_ms), (24000, None));
        let pinned = serde_json::to_value(Lease {
            key: LeaseKey {
                scope: Scope::Branch("develop".to_owned()),
                ..lease.key
            },
            port: 24000,
            last_used_ms: Some(5),
        })
        .unwrap();
        assert_eq!(pinned["branch"], "develop");
        assert!(pinned.get("worktree").is_none());
    }
}
