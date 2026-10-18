//! Leases: the port each Port name of each Service gets in each Worktree (ADR 0006).

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Mutex;

use tokio::net::TcpSocket;

use crate::ports::Range;
use crate::topology::Topology;

#[derive(Debug, thiserror::Error)]
pub(crate) enum LeaseError {
    #[error("no port of the Range {range} is free for Port name {port_name} of service {service}, widen settings.port_range")]
    RangeExhausted {
        service: String,
        port_name: String,
        range: Range,
    },
}

/// What a Lease binds: one Port name of one Service in one Worktree of one Repository.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct LeaseKey {
    /// The common git directory identifying the Repository.
    repository: PathBuf,
    worktree: PathBuf,
    service: String,
    port_name: String,
}

type Table = BTreeMap<LeaseKey, u16>;

/// Every Lease the Daemon granted, across all Repositories, so that no 2 of them share a port.
#[derive(Debug, Default)]
pub(crate) struct Leases {
    table: Mutex<Table>,
}

/// What a Service asks Leases for.
#[derive(Debug)]
pub(crate) struct LeaseRequest<'a> {
    pub(crate) topology: &'a Topology,
    pub(crate) service: &'a str,
    /// The declared port of each Port name of the Service.
    pub(crate) ports: &'a BTreeMap<String, u16>,
    pub(crate) range: Range,
    /// Every port the configuration declares, which the Range skips.
    pub(crate) declared: &'a BTreeSet<u16>,
}

impl Leases {
    /// The leased port of each Port name of the Service, granting the Leases it lacks. Blocking: it binds the
    /// candidate ports to check that nothing else holds them.
    pub(crate) fn grant(&self, request: &LeaseRequest<'_>) -> Result<BTreeMap<String, u16>, LeaseError> {
        let mut table = self
            .table
            .lock()
            .expect("the lease table mutex is never poisoned, no code panics while holding it");
        grant_in(&mut table, request, is_free)
    }
}

/// Grants the Leases of `request` in `table`, where `is_free` tells whether nothing outside Procly holds a port.
///
/// A Lease already granted is kept as it is, so the port stays the same across restarts. The Main worktree gets
/// its declared port whenever no other Lease and nothing else holds it, even when it held another one before, so
/// that editing the declared port takes effect.
fn grant_in(
    table: &mut Table,
    request: &LeaseRequest<'_>,
    is_free: impl Fn(u16) -> bool,
) -> Result<BTreeMap<String, u16>, LeaseError> {
    let mut granted = BTreeMap::new();
    for (port_name, &declared) in request.ports {
        let key = LeaseKey {
            repository: request.topology.git_common_dir.clone(),
            worktree: request.topology.worktree.path.clone(),
            service: request.service.to_owned(),
            port_name: port_name.clone(),
        };
        let current = table.get(&key).copied();
        let held_elsewhere = |port: u16| table.iter().any(|(other, &leased)| leased == port && *other != key);
        let port = if request.topology.worktree.main && current != Some(declared) && !held_elsewhere(declared) && is_free(declared)
        {
            declared
        } else if let Some(current) = current {
            current
        } else {
            request
                .range
                .ports()
                .find(|&port| !request.declared.contains(&port) && !held_elsewhere(port) && is_free(port))
                .ok_or_else(|| LeaseError::RangeExhausted {
                    service: request.service.to_owned(),
                    port_name: port_name.clone(),
                    range: request.range,
                })?
        };
        table.insert(key, port);
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
        Err(error) => matches!(
            error.raw_os_error(),
            Some(nix::libc::EAFNOSUPPORT | nix::libc::EADDRNOTAVAIL)
        ) || error.kind() == io::ErrorKind::Unsupported,
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

    const RANGE: Range = Range { start: 24000, end: 24009 };

    fn grant(table: &mut Table, topology: &Topology, ports: &[(&str, u16)], is_free: impl Fn(u16) -> bool) -> Vec<u16> {
        let ports: BTreeMap<String, u16> = ports.iter().map(|&(name, port)| (name.to_owned(), port)).collect();
        let declared = ports.values().copied().collect();
        let request = LeaseRequest {
            topology,
            service: "web",
            ports: &ports,
            range: RANGE,
            declared: &declared,
        };
        grant_in(table, &request, is_free).unwrap().into_values().collect()
    }

    #[test]
    fn the_main_worktree_gets_the_declared_port_and_another_worktree_the_first_of_the_range() {
        let mut table = Table::new();
        assert_eq!(grant(&mut table, &topology("/src/app", true), &[("http", 3000)], |_| true), [3000]);
        assert_eq!(grant(&mut table, &topology("/src/app-a", false), &[("http", 3000)], |_| true), [24000]);
        assert_eq!(grant(&mut table, &topology("/src/app-b", false), &[("http", 3000)], |_| true), [24001]);
    }

    #[test]
    fn a_lease_is_kept_even_once_its_port_is_busy() {
        let mut table = Table::new();
        let other = topology("/src/app-a", false);
        assert_eq!(grant(&mut table, &other, &[("http", 3000)], |_| true), [24000]);
        // The Service itself holds its port while it runs.
        assert_eq!(grant(&mut table, &other, &[("http", 3000)], |port| port != 24000), [24000]);
    }

    #[test]
    fn a_busy_port_is_skipped_and_the_main_worktree_falls_back_to_the_range() {
        let mut table = Table::new();
        let busy = |port| port != 3000 && port != 24000;
        assert_eq!(grant(&mut table, &topology("/src/app", true), &[("http", 3000)], busy), [24001]);
    }

    #[test]
    fn the_main_worktree_gets_its_declared_port_back_once_it_is_free() {
        let mut table = Table::new();
        let main = topology("/src/app", true);
        assert_eq!(grant(&mut table, &main, &[("http", 3000)], |port| port != 3000), [24000]);
        assert_eq!(grant(&mut table, &main, &[("http", 3000)], |_| true), [3000]);
        assert_eq!(grant(&mut table, &main, &[("http", 4000)], |_| true), [4000]);
    }

    #[test]
    fn each_port_name_gets_its_own_lease_and_the_range_skips_declared_ports() {
        let mut table = Table::new();
        let ports = [("http", 24000), ("test", 24001)];
        assert_eq!(grant(&mut table, &topology("/src/app-a", false), &ports, |_| true), [24002, 24003]);
        assert_eq!(grant(&mut table, &topology("/src/app", true), &ports, |_| true), [24000, 24001]);
    }

    #[test]
    fn a_full_range_is_an_error() {
        let mut table = Table::new();
        let ports = BTreeMap::from([("http".to_owned(), 3000)]);
        let request = LeaseRequest {
            topology: &topology("/src/app-a", false),
            service: "web",
            ports: &ports,
            range: RANGE,
            declared: &BTreeSet::new(),
        };
        assert!(matches!(
            grant_in(&mut table, &request, |_| false),
            Err(LeaseError::RangeExhausted { port_name, .. }) if port_name == "http"
        ));
    }

    #[test]
    fn a_port_held_by_a_listener_is_not_free() {
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(!is_free(port));
        drop(listener);
        assert!(is_free(port));
    }
}
