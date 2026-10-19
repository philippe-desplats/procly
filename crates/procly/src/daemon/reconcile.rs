//! Reconciliation: what a starting Daemon does with the Services a previous one recorded, so that a crash of the
//! Daemon never leaves a Service it cannot see running, to be started a second time, or an orphan holding a Lease.
//!
//! Each recorded process group is compared with the live system before anything is signalled. A leader that still
//! runs with the same start time is the recorded one, and is re-attached when it still listens on its Lease. A
//! leader that ended leaves a group that cannot belong to anyone else while a member lives, as the system keeps its
//! id reserved, so its members are orphans of the Service and are stopped. A pid now held by another process is
//! never signalled.

use super::listeners::listening_ports;
use super::process_group::{self, Identity, Pid};
use super::record::ServiceRecord;
use super::service::{Services, listens_on_lease};
use super::store::StoreError;
use crate::protocol::ServiceEnd;

/// What becomes of one recorded Service.
#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    Reattach,
    Ended(ServiceEnd),
}

/// Reconciles the Services recorded by the previous Daemon with the live system, and returns once every orphan is
/// gone, so that the port of its Lease is free for the next start.
pub(super) async fn run(services: &Services) -> Result<(), StoreError> {
    let records = services.records();
    let found = tokio::task::spawn_blocking(move || records.load())
        .await
        .expect("loading the service records never panics")?;
    let mut kept = Vec::new();
    let mut ended = Vec::new();
    for record in found {
        match judge(&record).await {
            Verdict::Reattach => kept.push(record),
            Verdict::Ended(end) => ended.push((record, end)),
        }
    }
    let (records, recorded) = (services.records(), kept.clone());
    // Before any of them is supervised, so that one ending at once is not recorded again.
    let _ = tokio::task::spawn_blocking(move || records.replace(recorded)).await;
    // The output file of a Service that ended while no Daemon ran is no one's, and no Service starts before this.
    let (outputs, followed) = (
        services.outputs().clone(),
        kept.iter()
            .filter_map(|record| record.output.clone())
            .collect::<Vec<_>>(),
    );
    let _ = tokio::task::spawn_blocking(move || outputs.sweep(&followed)).await;
    for record in kept {
        services.reattach(record);
    }
    for (record, end) in ended {
        services.forget(&record, end);
    }
    Ok(())
}

/// Compares one record with the live system, stopping what is left of a Service that cannot be re-attached.
async fn judge(record: &ServiceRecord) -> Verdict {
    let pgid = Pid::from_raw(record.pgid);
    let label = format!(
        "service {} of {}, process group {pgid},",
        record.service,
        record.worktree.path.display()
    );
    let leader = tokio::task::spawn_blocking(move || process_group::identity(pgid))
        .await
        .ok()
        .flatten();
    match leader {
        Some(leader) if is_recorded(record, &leader) => {
            // Nothing to probe, a Service without a port is healthy as long as it runs. A Group is probed on the port
            // of each Service it covers.
            let mut silent = None;
            for port in record.port.iter().chain(&record.covered_ports) {
                if !listens_on_lease(pgid, *port, &record.lease_ports).await {
                    silent = Some(*port);
                    break;
                }
            }
            let Some(port) = silent else {
                eprintln!("{label} still runs after a restart of the Daemon, it is re-attached");
                return Verdict::Reattach;
            };
            process_group::stop_group(pgid).await;
            eprintln!(
                "{label} still ran after a restart of the Daemon but no longer listened on its Lease, port {port}, its \
                 process group was stopped"
            );
            Verdict::Ended(ServiceEnd::NotListening { port })
        }
        Some(_) => {
            eprintln!("{label} ended while no Daemon ran, its pid now belongs to another process, which is left alone");
            Verdict::Ended(ServiceEnd::Lost)
        }
        None if process_group::group_exists(pgid) => {
            let lease_ports = record.lease_ports.clone();
            let held = tokio::task::spawn_blocking(move || listening_ports(pgid))
                .await
                .ok()
                .flatten()
                .and_then(|listening| listening.into_iter().find(|port| lease_ports.contains(port)));
            process_group::stop_group(pgid).await;
            let holding = held
                .map(|port| format!(" held port {port} of its Lease and"))
                .unwrap_or_default();
            eprintln!("{label} lost its leader while no Daemon ran, the rest of the group{holding} was stopped");
            Verdict::Ended(ServiceEnd::Orphaned {
                pgid: record.pgid,
                port: held,
            })
        }
        None => {
            eprintln!("{label} ended while no Daemon ran");
            Verdict::Ended(ServiceEnd::Lost)
        }
    }
}

/// Whether the live process `leader` is the leader the record names, and not one that got its pid since.
fn is_recorded(record: &ServiceRecord, leader: &Identity) -> bool {
    leader.pgid.as_raw() == record.pgid && record.started.as_ref() == Some(&leader.started)
}
