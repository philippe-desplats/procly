//! `procly doctor`, the side that touches the state of the Daemon: the checks only a running Daemon can answer, and the
//! readers of the state directory that the client runs itself, so that an unreachable Daemon is still reported.
//!
//! Nothing here changes anything. A check names the command that fixes what it found and never runs it.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::hook::{self, Standing};
use super::managed::ManagedBlocks;
use super::store::StoreError;
use super::{
    Registry, agent, blocking, container_record, foreign, gc, index, isolated_record, lease, managed, off, record,
    resource,
};
use crate::protocol::{Check, DaemonChecks, Occupant, Response, ServiceEnd, ServiceState};
use crate::state_dir::StateDir;

/// The check of the state documents.
pub(crate) const STATE_DOCUMENTS: &str = "state_documents";
/// The check of the Guard in the Worktree.
pub(crate) const GUARD: &str = "guard";
/// The check of the ports Leases hold.
const LEASED_PORTS: &str = "leased_ports";
/// The check of the Orphans.
const ORPHANS: &str = "orphans";
/// The checks only the Daemon answers, which `doctor` reports as skipped when it does not.
pub(crate) const DAEMON_CHECKS: [&str; 2] = [LEASED_PORTS, ORPHANS];

/// Answers a `Doctor` request with the version of this Daemon and the checks only it can answer.
pub(super) async fn run(registry: &Arc<Registry>) -> Response {
    let reconciled = registry.reconciled().await;
    let registry = Arc::clone(registry);
    // Reading the documents and asking git, Docker and `lsof` are blocking.
    let checks = blocking("the checks could not run", move || Ok(checks(&registry, reconciled))).await;
    match checks {
        Ok(checks) => Response::Doctor(DaemonChecks {
            procly_version: env!("CARGO_PKG_VERSION").to_owned(),
            checks,
        }),
        Err(message) => Response::Error { message },
    }
}

/// The checks only the Daemon can answer, in the order `doctor` lists them. A later check is appended to the array and
/// to the call that fills it.
fn checks(registry: &Registry, reconciled: Result<(), String>) -> Vec<Check> {
    match reconciled {
        Ok(()) => vec![leased_ports(registry), orphans(registry)],
        // What it recorded is unreadable, which the check of the state documents reports.
        Err(error) => DAEMON_CHECKS
            .into_iter()
            .map(|name| {
                Check::skipped(
                    name,
                    format!("the Daemon cannot read what it recorded, see {STATE_DOCUMENTS}: {error}"),
                )
            })
            .collect(),
    }
}

/// Whether a Service ended on a Port mismatch, and whether something Procly did not start holds a port a Lease holds.
fn leased_ports(registry: &Registry) -> Check {
    let mut problems = Vec::new();
    let mut fixes = Vec::new();
    for (topology, status) in registry.services.statuses() {
        if let ServiceState::Ended {
            end: ServiceEnd::PortMismatch { leased, actual },
        } = status.state
        {
            let worktree = topology.worktree.path.display();
            problems.push(format!(
                "service {} of {worktree} listened on port {actual} instead of its Lease, port {leased} (Port mismatch)",
                status.service
            ));
            fixes.push(format!("cd {worktree} && procly env {}", status.service));
        }
    }
    let leases = match registry.leases.bound_to_worktrees() {
        Ok(leases) => leases,
        Err(error) => return Check::skipped(LEASED_PORTS, format!("{:#}", anyhow::Error::from(error))),
    };
    let running = registry.running();
    for lease in leases {
        if running.contains(&(lease.worktree.clone(), lease.service.clone())) {
            continue;
        }
        let Some((occupant, stop)) = foreign::occupant(lease.port, None) else {
            continue;
        };
        problems.push(format!(
            "lease {}.{} port {} of {} is held by {}, which Procly did not start",
            lease.service,
            lease.port_name,
            lease.port,
            lease.worktree.display(),
            occupant_text(&occupant),
        ));
        match (stop, &occupant) {
            (Some(stop), _) => fixes.push(stop),
            (None, Occupant::Unknown { find }) => fixes.push(find.clone()),
            (None, _) => {}
        }
    }
    if problems.is_empty() {
        return Check::passed(
            LEASED_PORTS,
            "every leased port is free or held by the Service it is leased to",
        );
    }
    Check::failed(
        LEASED_PORTS,
        problems.join("\n"),
        (!fixes.is_empty()).then(|| fixes.join("\n")),
    )
}

/// What holds a port, as the message of a check tells it.
fn occupant_text(occupant: &Occupant) -> String {
    match occupant {
        Occupant::Container { name, .. } => format!("container {name}"),
        Occupant::Process { pid, command } => format!("process {pid} {command}"),
        Occupant::Unknown { .. } => "something Procly cannot name".to_owned(),
    }
}

/// Whether removed Worktrees left something behind, which `procly gc` lists.
fn orphans(registry: &Registry) -> Check {
    match gc::orphans(registry) {
        Ok(report) if report.orphans.is_empty() => Check::passed(ORPHANS, "no removed Worktree left anything behind"),
        Ok(report) => {
            let count = report.orphans.len();
            Check::failed(
                ORPHANS,
                format!(
                    "{count} {} left by removed Worktrees",
                    if count == 1 { "Orphan" } else { "Orphans" }
                ),
                Some("procly gc".to_owned()),
            )
        }
        Err(error) => Check::skipped(ORPHANS, format!("{error:#}")),
    }
}

/// Whether every document of the state directory can be read, as the next request that needs it reads it.
pub(crate) fn state_documents(state: &StateDir) -> Check {
    type Readable = fn(&Path) -> Result<(), StoreError>;
    let documents: [(PathBuf, Readable); 8] = [
        (state.leases(), lease::readable),
        (state.indexes(), index::readable),
        (state.resources(), resource::readable),
        (state.services(), record::readable),
        (state.containers(), container_record::readable),
        (state.isolated(), isolated_record::readable),
        (state.off(), off::readable),
        (state.managed_blocks(), managed::readable),
    ];
    let mut problems = String::new();
    let mut fixes = Vec::new();
    for (path, readable) in documents {
        let Err(error) = readable(&path) else { continue };
        if !problems.is_empty() {
            problems.push('\n');
        }
        // A document written by a newer procly is not broken, and moving it aside would lose what that one recorded.
        let newer = matches!(error, StoreError::Version { .. });
        let _ = write!(problems, "{:#}", anyhow::Error::from(error));
        if !newer {
            fixes.push(format!("mv {} {}.broken", path.display(), path.display()));
        }
    }
    if problems.is_empty() {
        return Check::passed(STATE_DOCUMENTS, "every state document can be read");
    }
    Check::failed(STATE_DOCUMENTS, problems, (!fixes.is_empty()).then(|| fixes.join("\n")))
}

/// Whether the Guard of the Worktree `worktree` is in place as Procly recorded it. `unplaced` says why there is no
/// Worktree when there is none.
pub(crate) fn guard(state: &StateDir, worktree: Option<&Path>, unplaced: &str) -> Check {
    let Some(worktree) = worktree else {
        return Check::skipped(GUARD, unplaced);
    };
    let file = worktree.join(agent::SETTINGS);
    let install = Some("procly agent install".to_owned());
    match hook::standing(&ManagedBlocks::new(state.managed_blocks()), &file) {
        Ok(Standing::Intact) => Check::passed(
            GUARD,
            format!("{} registers the Guard as Procly wrote it", file.display()),
        ),
        Ok(Standing::Absent) => Check::skipped(
            GUARD,
            format!(
                "{} holds no Guard, `procly agent install` writes it if you want your agents steered",
                file.display()
            ),
        ),
        Ok(Standing::Missing) => Check::failed(
            GUARD,
            format!("{} no longer registers the Guard Procly wrote", file.display()),
            install,
        ),
        Ok(Standing::Edited) => Check::failed(
            GUARD,
            format!(
                "the Guard in {} is not the one Procly recorded, `procly agent install` leaves it as it is, \
                 restore the hook group running `{}` by hand",
                file.display(),
                hook::COMMAND
            ),
            None,
        ),
        Err(error) => Check::failed(GUARD, format!("{:#}", anyhow::Error::from(error)), None),
    }
}
