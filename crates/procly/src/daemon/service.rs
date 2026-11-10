//! Services: declared Process services, run with pipes in a process group of their own and health-checked.

use std::collections::{BTreeSet, HashMap};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use process_wrap::tokio::{ChildWrapper, CommandWrap};
use tokio::sync::watch;

use super::clock::Clock;
use super::cpu::groups_cpu;
use super::health::HealthCheck;
use super::idle::{IdleTracker, Sample};
use super::listeners::listening_ports;
use super::output_file::{self, OutputFiles, Tail};
use super::process_group::{self, Pid, Tree, kill_tree, stop_detached, stop_tree};
use super::record::{ServiceRecord, ServiceRecords};
use crate::config::RestartPolicy;
use crate::output::{self, Follow, OutputBuffer};
use crate::protocol::{self, ServiceEnd, ServiceStatus, StartOutcome};
use crate::topology::Topology;

/// How long the health check waits between 2 probes.
const HEALTH_INTERVAL: Duration = Duration::from_millis(50);
/// How long the health check waits after a probe was answered while the tree of the Service does not listen on the Lease
/// yet, which is what a Foreign process holding the Lease looks like for as long as the health timeout lasts. Reading
/// the tree costs a listing of the processes of the system and an `lsof`, which at [`HEALTH_INTERVAL`] would run 20
/// times a second for a minute.
const OWNERSHIP_RETRY: Duration = Duration::from_millis(250);
/// How often the Daemon looks at a Service to tell whether it is idle.
const IDLE_INTERVAL: Duration = Duration::from_secs(10);
/// How old a reading of the processes may be for a look at idleness, which tells 10 seconds apart: the Services that
/// look within the same second share one reading of the processes and one of their CPU time.
const IDLE_SHARED: Duration = Duration::from_secs(1);
/// How often the Daemon looks whether the process of a re-attached Service, which is not its child, still runs.
const LIVENESS_INTERVAL: Duration = Duration::from_millis(100);
/// How many looks at the leader of a re-attached Service pass between 2 tries to judge it again, when it was kept
/// because the processes could not be listed: once a second.
const PROVE_EVERY: u32 = 10;
/// How long a restarted Service must stay healthy for the next crash to count as a first one again, instead of
/// one more in a row.
const STABLE_RUN: Duration = Duration::from_mins(1);

#[derive(Debug, thiserror::Error)]
pub(crate) enum ServiceError {
    #[error("cannot start service {service} with {command:?} in {cwd}")]
    Spawn {
        service: String,
        command: String,
        cwd: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cannot create the output file of service {service} in {dir}")]
    Output {
        service: String,
        dir: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("service {service} started without a process id")]
    NoPid { service: String },
    #[error(
        "cannot start group {group}, the services it covers run on their own: {}, stop them first with {}",
        .services.join(", "),
        .services.iter().map(|service| format!("`procly down {service}`")).collect::<Vec<_>>().join(" and ")
    )]
    CoveredRunning { group: String, services: Vec<String> },
}

/// What a Service runs, read from the configuration.
#[derive(Debug)]
pub(crate) struct ServiceSpec {
    pub(crate) name: String,
    pub(crate) command: String,
    pub(crate) cwd: PathBuf,
    pub(crate) env: Vec<(String, String)>,
    pub(crate) health: HealthCheck,
    pub(crate) health_timeout: Duration,
    /// The root of the Worktree the Service runs in, named in the Daemon log.
    pub(crate) worktree: PathBuf,
    /// The Repository and the Worktree the Service runs in, recorded for the status and for the next Daemon.
    pub(crate) topology: Topology,
    pub(crate) restart: RestartPolicy,
    /// How long the Service may be idle before it is stopped, `None` to leave it running however long it is.
    pub(crate) idle: Option<Duration>,
    /// The port of every Lease of the Service, which may listen on all of them.
    pub(crate) lease_ports: Vec<u16>,
    /// The Services it covers when it is a Group, whose Leases it holds.
    pub(crate) covers: Vec<String>,
}

/// How a Service relates to the Groups of its Worktree when it is started.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Relation<'a> {
    /// Neither a Group nor covered by one.
    Alone,
    /// A Group covering these Services, none of which may run on its own.
    Group { covers: &'a [String] },
    /// Covered by this Group, whose running answers a start request with a Reuse.
    Covered { by: &'a str },
}

/// A Service runs at most once per Worktree: it is keyed by the root of its Worktree and its name.
type ServiceKey = (PathBuf, String);

/// The Service stored under `key` when it has not ended, the one a start request must find rather than replace.
fn running_at(table: &HashMap<ServiceKey, Arc<Service>>, key: &ServiceKey) -> Option<Arc<Service>> {
    table.get(key).filter(|service| service.is_running()).map(Arc::clone)
}

/// Every Service the Daemon started, running or ended.
#[derive(Debug)]
pub(crate) struct Services {
    clock: Clock,
    records: Arc<ServiceRecords>,
    outputs: OutputFiles,
    table: Mutex<HashMap<ServiceKey, Arc<Service>>>,
    /// Held from the lookup to the insert, so that 2 concurrent requests never both start the Service.
    starting: Mutex<()>,
}

#[derive(Debug)]
pub(crate) struct Service {
    port: Option<u16>,
    topology: Topology,
    /// The Services it covers when it is a Group.
    covers: Vec<String>,
    /// Whether a previous Daemon started the Service, and this one found it running when it started.
    reattached: bool,
    /// When the Service last started, in milliseconds since the Unix epoch on the clock of the Daemon. Set again by
    /// each automatic restart. For a Service a previous Daemon started, when this Daemon found it.
    started_at: AtomicU64,
    /// The output of every run of the Service, restarts included.
    output: Arc<Mutex<OutputBuffer>>,
    state: watch::Sender<ServiceState>,
    /// Set once to ask the task watching the process to stop its tree, with the end to report once it is gone.
    stop: watch::Sender<Option<ServiceEnd>>,
    /// The processes of the current run: the leader and everything it started, in whatever process group.
    tree: Tree,
    /// What a Service kept without a process list still has to show once the processes can be listed. `None` for a
    /// Service that needs no more proof, and once it was given.
    unproven: tokio::sync::Mutex<Option<Unproven>>,
}

/// A Service found running by a Daemon that could not list the processes, with something answering on its Lease: kept
/// as it was, though nothing showed that its tree is what answers.
#[derive(Debug)]
struct Unproven {
    /// Names the Service in the Daemon log.
    label: String,
    /// The port that was in doubt.
    port: u16,
    /// The ports its tree must listen on: its own and those of the Services it covers.
    ports: Vec<u16>,
    lease_ports: Vec<u16>,
}

/// What judging a Service again, once its processes can be listed, came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Proof {
    /// There is nothing left to prove: the tree listens on the Lease, or the Service was never in doubt.
    Done,
    /// The tree no longer listens on the Lease, and the Service is being stopped as a re-attach does when it finds so.
    Gone,
    /// The processes still cannot be listed, so the Service is only kept as it was, and the port is in doubt.
    Unreadable(u16),
}

/// The outcome of a start request.
#[derive(Debug)]
pub(crate) enum Start {
    /// The Service was started, or found running, and its health is pending or known.
    Ready {
        service: Arc<Service>,
        outcome: StartOutcome,
        /// The Group running the Service that was asked for, which is covered by it.
        via: Option<String>,
    },
    /// The Service is being stopped: wait for [`Service::ended`], then ask again.
    Stopping(Arc<Service>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ServiceState {
    Starting,
    Healthy,
    /// The process crashed and its restart is waiting out the delay.
    Restarting,
    Ended(ServiceEnd),
}

impl Services {
    pub(crate) fn new(clock: Clock, records: ServiceRecords, outputs: OutputFiles) -> Self {
        Self {
            clock,
            records: Arc::new(records),
            outputs,
            table: Mutex::default(),
            starting: Mutex::default(),
        }
    }

    /// Returns the running Service `name` of `worktree`, or spawns it from the spec built by `spec`, without
    /// waiting for it to be healthy. A Service being stopped is no Reuse, it still holds its port until it ends.
    ///
    /// A Service covered by a Group that runs is the Group's, and a Group does not start over a Service it covers
    /// that runs on its own. Both are decided under the start mutex, so that no 2 requests start one of each.
    /// Must run inside the Tokio runtime.
    pub(crate) fn start<E: From<ServiceError>>(
        &self,
        worktree: &Path,
        name: &str,
        relation: Relation<'_>,
        spec: impl FnOnce() -> Result<ServiceSpec, E>,
    ) -> Result<Start, E> {
        // Held through the configuration read and the spawn, on a blocking thread and never across an await.
        let _starting = self
            .starting
            .lock()
            .expect("the start mutex is never poisoned, no code panics while holding it");
        let key = (worktree.to_owned(), name.to_owned());
        // A Service covered by a Group runs either on its own, under its own name, or through the Group.
        let running = {
            let table = self.table();
            let own = running_at(&table, &key).map(|service| (service, None));
            match relation {
                Relation::Covered { by } => own.or_else(|| {
                    running_at(&table, &(worktree.to_owned(), by.to_owned())).map(|group| (group, Some(by.to_owned())))
                }),
                Relation::Alone | Relation::Group { .. } => own,
            }
        };
        if let Some((service, via)) = running {
            return Ok(if service.is_stopping() {
                Start::Stopping(service)
            } else {
                Start::Ready {
                    service,
                    outcome: StartOutcome::Reused,
                    via,
                }
            });
        }
        if let Relation::Group { covers } = relation {
            let table = self.table();
            let alone: Vec<String> = covers
                .iter()
                .filter(|covered| {
                    table
                        .get(&(worktree.to_owned(), (*covered).clone()))
                        .is_some_and(|service| service.is_running())
                })
                .cloned()
                .collect();
            if !alone.is_empty() {
                return Err(ServiceError::CoveredRunning {
                    group: name.to_owned(),
                    services: alone,
                }
                .into());
            }
        }
        // Every relation looked the Service up under its own key above, so an entry still running there is a bug in
        // that lookup. Starting over it would replace the entry, and the Daemon would lose the instance that runs:
        // `down` and `daemon stop` walk the table, and the supervisor of the instance would restart it unseen.
        if let Some(service) = running_at(&self.table(), &key) {
            eprintln!(
                "service {name} of {} already runs and a start request found no Reuse for it, this is a bug: the running one is kept and nothing is started",
                worktree.display()
            );
            return Ok(if service.is_stopping() {
                Start::Stopping(service)
            } else {
                Start::Ready {
                    service,
                    outcome: StartOutcome::Reused,
                    via: None,
                }
            });
        }
        let service = spawn(
            spec()?,
            self.clock.clone(),
            Arc::clone(&self.records),
            self.outputs.clone(),
        )?;
        let mut table = self.table();
        if let Relation::Group { covers } = relation {
            // The Group now stands for the Services it covers: an ended row of one would offer a Restart that restarts
            // the whole Group. None of them runs, the check above refused the start otherwise.
            table.retain(|(path, service_name), _| path != worktree || !covers.contains(service_name));
        }
        table.insert(key, Arc::clone(&service));
        drop(table);
        Ok(Start::Ready {
            service,
            outcome: StartOutcome::Started,
            via: None,
        })
    }

    /// The recent output of the Service `name` of `worktree`, running or ended, or else of the running Group covering
    /// it, whose process prints for it. `None` when the Daemon knows no such Service.
    pub(crate) fn output_of(&self, worktree: &Path, name: &str) -> Option<String> {
        self.find(worktree, name).map(|service| service.output())
    }

    /// Follows the output of the Service `name` of `worktree`, found as [`Services::output_of`] finds it, and returns
    /// the Service whose end ends the follow.
    pub(crate) fn follow(&self, worktree: &Path, name: &str) -> Option<(Arc<Service>, Follow)> {
        self.find(worktree, name).map(|service| {
            let follow = output::follow(&service.output);
            (service, follow)
        })
    }

    /// The Service `name` of `worktree`, running or ended, or else the running Group covering it.
    fn find(&self, worktree: &Path, name: &str) -> Option<Arc<Service>> {
        let table = self.table();
        table
            .get(&(worktree.to_owned(), name.to_owned()))
            .or_else(|| {
                table
                    .iter()
                    .find(|((path, _), service)| {
                        path == worktree && service.is_running() && service.covers.iter().any(|covered| covered == name)
                    })
                    .map(|(_, group)| group)
            })
            .map(Arc::clone)
    }

    /// Whether the Service `name` of `worktree` runs.
    pub(crate) fn runs(&self, worktree: &Path, name: &str) -> bool {
        self.table()
            .get(&(worktree.to_owned(), name.to_owned()))
            .is_some_and(|service| service.is_running())
    }

    /// Asks every running Service of `worktree`, or only the one named `name`, to stop, and returns them sorted by
    /// name. Wait for each with [`Service::ended`].
    pub(crate) fn stop(&self, worktree: &Path, name: Option<&str>) -> Vec<(String, Arc<Service>)> {
        let stopping = self.running_in(worktree, name);
        for (_, service) in &stopping {
            service.request_stop(ServiceEnd::Stopped);
        }
        stopping
    }

    /// The names of the Services of `worktree` that [`Services::stop`] would ask to stop, sorted, without asking.
    pub(crate) fn would_stop(&self, worktree: &Path) -> Vec<String> {
        self.running_in(worktree, None)
            .into_iter()
            .map(|(name, _)| name)
            .collect()
    }

    /// Every running Service of `worktree`, or only the one named `name`, sorted by name.
    fn running_in(&self, worktree: &Path, name: Option<&str>) -> Vec<(String, Arc<Service>)> {
        let mut running: Vec<_> = self
            .table()
            .iter()
            .filter(|((path, service_name), service)| {
                path == worktree && name.is_none_or(|name| name == service_name) && service.is_running()
            })
            .map(|((_, service_name), service)| (service_name.clone(), Arc::clone(service)))
            .collect();
        running.sort_by(|(a, _), (b, _)| a.cmp(b));
        running
    }

    /// Forgets every ended Service of `worktree`, for a Worktree release that has stopped what ran. A Service that
    /// runs stays, and is keyed by its name again once started.
    pub(crate) fn remove_ended_in(&self, worktree: &Path) {
        self.table()
            .retain(|(path, _), service| path != worktree || service.is_running());
    }

    /// Asks every running Service of the Repository whose common git directory is `git_common_dir` to stop, whatever
    /// its Worktree, including one git no longer lists. Returns them sorted by Worktree and name, to wait for with
    /// [`Service::ended`].
    pub(crate) fn stop_repository(&self, git_common_dir: &Path) -> Vec<(ServiceKey, Arc<Service>)> {
        let mut stopping: Vec<_> = self
            .table()
            .iter()
            .filter(|(_, service)| service.topology.git_common_dir == git_common_dir && service.is_running())
            .map(|(key, service)| (key.clone(), Arc::clone(service)))
            .collect();
        stopping.sort_by(|(a, _), (b, _)| a.cmp(b));
        for (_, service) in &stopping {
            service.request_stop(ServiceEnd::Stopped);
        }
        stopping
    }

    /// The key of every running Service of the Repository whose common git directory is `git_common_dir` that runs for
    /// another Worktree than `worktree`, sorted by Worktree and name, for an eject of one Worktree to say it spared them.
    pub(crate) fn running_outside(&self, git_common_dir: &Path, worktree: &Path) -> Vec<ServiceKey> {
        let mut running: Vec<_> = self
            .table()
            .iter()
            .filter(|((path, _), service)| {
                path != worktree && service.topology.git_common_dir == git_common_dir && service.is_running()
            })
            .map(|(key, _)| key.clone())
            .collect();
        running.sort();
        running
    }

    /// The Worktree root and the name of every running Service, a running Group counting for the Services it covers
    /// too, as it holds their Leases.
    pub(crate) fn running(&self) -> BTreeSet<ServiceKey> {
        self.table()
            .iter()
            .filter(|(_, service)| service.is_running())
            .flat_map(|((worktree, name), service)| {
                let covered = service.covers.iter().map(|covered| (worktree.clone(), covered.clone()));
                std::iter::once((worktree.clone(), name.clone())).chain(covered)
            })
            .collect()
    }

    /// Stops the tree of every running Service and waits until they are gone, for a Daemon about to exit.
    pub(crate) async fn stop_all(&self) {
        let running: Vec<_> = self
            .table()
            .values()
            .filter(|service| service.is_running())
            .cloned()
            .collect();
        for service in &running {
            service.request_stop(ServiceEnd::Stopped);
        }
        for service in running {
            service.ended().await;
        }
    }

    /// The process group of every running Service, and those of the tasks they started in groups of their own. Reads the
    /// processes of the system once for all of them. Blocking.
    pub(crate) fn process_groups(&self) -> Vec<i32> {
        let mut groups = self.records.process_groups();
        let trees: Vec<Tree> = self
            .table()
            .values()
            .filter(|service| service.is_running())
            .map(|service| service.tree.clone())
            .collect();
        for tree in &trees {
            tree.refresh();
        }
        groups.extend(trees.iter().flat_map(Tree::groups).map(Pid::as_raw));
        groups
    }

    /// The records of the running Services, which a new Daemon reconciles before it starts any.
    pub(crate) fn records(&self) -> Arc<ServiceRecords> {
        Arc::clone(&self.records)
    }

    /// The output files of the running Services.
    pub(crate) fn outputs(&self) -> &OutputFiles {
        &self.outputs
    }

    /// Takes over the Service of `record`, which a previous Daemon started and which still runs and listens on its
    /// Lease: it is healthy, and supervised through its process group, as it is no child of this Daemon. Its output
    /// file is followed from its start, so its output from before the restart is kept. Must run inside the Tokio
    /// runtime.
    ///
    /// `tree` is the one the reconciliation just built from the record. `doubt` is the port that answered while the
    /// Daemon could not tell what listens on it: the Service is judged again as soon as the processes can be listed.
    pub(crate) fn reattach(&self, record: ServiceRecord, tree: Tree, doubt: Option<u16>) {
        let output = Arc::new(Mutex::new(OutputBuffer::dated(output::CAPACITY, self.clock.clone())));
        // A Service recorded before output files existed wrote to a pipe, its output is lost.
        let tail = record.output.as_ref().and_then(|path| match output_file::open(path) {
            // The writer of a Service re-attached after a restart belongs to no Daemon.
            Ok(reader) => Some(Tail::follow(path.clone(), reader, Arc::clone(&output), None)),
            Err(error) => {
                eprintln!("cannot read the output file {}: {error}", path.display());
                None
            }
        });
        let service = Arc::new(Service {
            port: record.port,
            topology: record.topology(),
            covers: record.covers.clone(),
            reattached: true,
            started_at: AtomicU64::new(epoch_millis(&self.clock)),
            output,
            state: watch::Sender::new(ServiceState::Healthy),
            stop: watch::Sender::new(None),
            tree,
            unproven: tokio::sync::Mutex::new(doubt.map(|port| Unproven {
                label: format!("service {} of {}", record.service, record.worktree.path.display()),
                port,
                ports: record.port.iter().chain(&record.covered_ports).copied().collect(),
                lease_ports: record.lease_ports.clone(),
            })),
        });
        self.table().insert(
            (record.worktree.path.clone(), record.service.clone()),
            Arc::clone(&service),
        );
        tokio::spawn(supervise_reattached(
            service,
            record,
            tail,
            self.clock.clone(),
            Arc::clone(&self.records),
        ));
    }

    /// Reports the Service of `record`, which a previous Daemon started, as ended with `end`, so that the status
    /// tells what became of it.
    pub(crate) fn forget(&self, record: &ServiceRecord, end: ServiceEnd) {
        let service = Arc::new(Service {
            port: record.port,
            topology: record.topology(),
            covers: record.covers.clone(),
            reattached: false,
            started_at: AtomicU64::new(epoch_millis(&self.clock)),
            output: Arc::new(Mutex::new(OutputBuffer::new(output::CAPACITY))),
            state: watch::Sender::new(ServiceState::Ended(end)),
            stop: watch::Sender::new(None),
            tree: Tree::default(),
            unproven: tokio::sync::Mutex::default(),
        });
        self.table()
            .insert((record.worktree.path.clone(), record.service.clone()), service);
    }

    /// Every Service with its Topology, sorted by Worktree and name.
    pub(crate) fn statuses(&self) -> Vec<(Topology, ServiceStatus)> {
        let mut statuses: Vec<_> = self
            .table()
            .iter()
            .map(|((worktree, name), service)| {
                let state = match *service.state.borrow() {
                    ServiceState::Starting => protocol::ServiceState::Starting,
                    ServiceState::Healthy => protocol::ServiceState::Healthy,
                    ServiceState::Restarting => protocol::ServiceState::Restarting,
                    ServiceState::Ended(end) => protocol::ServiceState::Ended { end },
                };
                let status = ServiceStatus {
                    service: name.clone(),
                    port: service.port,
                    reattached: service.reattached,
                    started_at: service.started_at.load(Ordering::Relaxed),
                    // The Services know nothing of the held environments, the status report tells.
                    restartable: false,
                    state,
                };
                ((worktree.clone(), name.clone()), service.topology.clone(), status)
            })
            .collect();
        statuses.sort_by(|(a, ..), (b, ..)| a.cmp(b));
        statuses
            .into_iter()
            .map(|(_, topology, status)| (topology, status))
            .collect()
    }

    fn table(&self) -> std::sync::MutexGuard<'_, HashMap<ServiceKey, Arc<Service>>> {
        self.table
            .lock()
            .expect("the service table mutex is never poisoned, no code panics while holding it")
    }
}

impl Service {
    pub(crate) fn port(&self) -> Option<u16> {
        self.port
    }

    /// Waits until the Service is healthy, restarts included, or returns how it ended before.
    pub(crate) async fn healthy(&self) -> Result<(), ServiceEnd> {
        let mut state = self.state.subscribe();
        let state = *state
            .wait_for(|state| matches!(state, ServiceState::Healthy | ServiceState::Ended(_)))
            .await
            .expect("the service holds its own state sender, so the channel never closes");
        match state {
            ServiceState::Healthy => Ok(()),
            ServiceState::Ended(end) => Err(end),
            ServiceState::Starting | ServiceState::Restarting => {
                unreachable!("wait_for returned on a state that is neither Healthy nor Ended")
            }
        }
    }

    /// Waits until the Service has ended and its tree is gone.
    pub(crate) async fn ended(&self) {
        let _ = self
            .state
            .subscribe()
            .wait_for(|state| matches!(state, ServiceState::Ended(_)))
            .await
            .expect("the service holds its own state sender, so the channel never closes");
    }

    pub(crate) fn output(&self) -> String {
        output::lock(&self.output).text()
    }

    /// Judges a Service that was kept without a process list, if it still is one, as soon as the processes can be listed:
    /// the same judgement a re-attach makes, from a tree that is now known. A tree that listens on its Lease stays and is
    /// a Service like any other. One that does not is stopped, which signals nothing but its own processes, so that a
    /// Foreign process holding the port is left alone and shown as Foreign.
    pub(crate) async fn prove(&self) -> Proof {
        let mut unproven = self.unproven.lock().await;
        let Some(pending) = unproven.as_ref() else {
            // A judgement made just before, by the task watching the Service, may have found it gone.
            return if self.is_stopping() { Proof::Gone } else { Proof::Done };
        };
        let tree = self.tree.clone();
        if !tokio::task::spawn_blocking(move || tree.refresh())
            .await
            .unwrap_or(false)
        {
            return Proof::Unreadable(pending.port);
        }
        for port in &pending.ports {
            if listens_on_lease(&self.tree, *port, &pending.lease_ports).await == LeaseHeld::No {
                eprintln!(
                    "{} no longer listened on its Lease, port {port}, when the Daemon could list the processes again, \
                     its tree was stopped",
                    pending.label
                );
                self.request_stop(ServiceEnd::NotListening { port: *port });
                *unproven = None;
                return Proof::Gone;
            }
        }
        eprintln!(
            "{} listens on its Lease, port {}: the Daemon can list the processes again, it is re-attached",
            pending.label, pending.port
        );
        *unproven = None;
        Proof::Done
    }

    fn is_running(&self) -> bool {
        !matches!(*self.state.borrow(), ServiceState::Ended(_))
    }

    fn is_stopping(&self) -> bool {
        self.stop.borrow().is_some()
    }

    /// Asks the supervisor to stop the tree, keeping the reason of the first request.
    fn request_stop(&self, end: ServiceEnd) {
        self.stop.send_if_modified(|stop| {
            let first = stop.is_none();
            if first {
                *stop = Some(end);
            }
            first
        });
    }
}

/// One run of the process of a Service.
struct Process {
    child: Box<dyn ChildWrapper>,
    /// The leader of the process group holding the tree, so its pid is also the group id.
    pgid: Pid,
    /// Follows the output file the run appends to.
    tail: Tail,
}

fn spawn(
    spec: ServiceSpec,
    clock: Clock,
    records: Arc<ServiceRecords>,
    outputs: OutputFiles,
) -> Result<Arc<Service>, ServiceError> {
    let output = Arc::new(Mutex::new(OutputBuffer::dated(output::CAPACITY, clock.clone())));
    let tree = Tree::default();
    let process = launch(&spec, &output, &outputs, &tree)?;
    // Recorded before anyone hears of the Service, so that a crash of the Daemon from now on leaves a trace of it.
    record_run(&records, record_of(&spec, process.tail.path()), process.pgid, &tree);
    let service = Arc::new(Service {
        port: spec.health.port,
        topology: spec.topology.clone(),
        covers: spec.covers.clone(),
        reattached: false,
        started_at: AtomicU64::new(epoch_millis(&clock)),
        output,
        state: watch::Sender::new(ServiceState::Starting),
        stop: watch::Sender::new(None),
        tree,
        unproven: tokio::sync::Mutex::default(),
    });
    tokio::spawn(supervise(Arc::clone(&service), spec, clock, records, outputs, process));
    Ok(service)
}

/// The record of a Service whose current run appends to the output file `output`, waiting for its process group.
fn record_of(spec: &ServiceSpec, output: &Path) -> ServiceRecord {
    ServiceRecord {
        repository: spec.topology.git_common_dir.clone(),
        worktree: spec.topology.worktree.clone(),
        service: spec.name.clone(),
        pgid: 0,
        started: None,
        port: spec.health.port,
        lease_ports: spec.lease_ports.clone(),
        covers: spec.covers.clone(),
        covered_ports: spec.health.covered.iter().filter_map(|covered| covered.port).collect(),
        idle_ms: spec
            .idle
            .map(|idle| u64::try_from(idle.as_millis()).unwrap_or(u64::MAX)),
        output: Some(output.to_owned()),
    }
}

/// Records the run of the process group `pgid` in `record`, and tells `tree` when its leader started, so that the tree
/// never has to wait for a listing to know which process its leader is. Blocking: it reads when the leader started with
/// `ps` and writes the document.
fn record_run(records: &ServiceRecords, mut record: ServiceRecord, pgid: Pid, tree: &Tree) {
    record.pgid = pgid.as_raw();
    record.started = process_group::identity(pgid).map(|identity| identity.started);
    tree.note_start(record.started.clone());
    records.put(record);
}

/// Records the run `process` of a restarted Service, on a blocking thread as writing the document syncs it to disk.
async fn record_restart(records: &Arc<ServiceRecords>, spec: &ServiceSpec, process: &Process, tree: &Tree) {
    let (records, record, pgid, tree) = (
        Arc::clone(records),
        record_of(spec, process.tail.path()),
        process.pgid,
        tree.clone(),
    );
    let _ = tokio::task::spawn_blocking(move || record_run(&records, record, pgid, &tree)).await;
}

/// Ends the Service with `end`, once it is no longer recorded, so that a Daemon starting right after never finds a
/// Service that ended.
async fn finish(service: &Service, records: &Arc<ServiceRecords>, topology: &Topology, name: &str, end: ServiceEnd) {
    let (records, worktree, name) = (Arc::clone(records), topology.worktree.path.clone(), name.to_owned());
    // Writing the document syncs it to disk, which must not park a runtime worker.
    let _ = tokio::task::spawn_blocking(move || records.remove(worktree, name)).await;
    service.state.send_replace(ServiceState::Ended(end));
}

/// Starts the process of the Service, appending its output to a new output file of `outputs`, which is followed into
/// `output`.
fn launch(
    spec: &ServiceSpec,
    output: &Arc<Mutex<OutputBuffer>>,
    outputs: &OutputFiles,
    tree: &Tree,
) -> Result<Process, ServiceError> {
    let file = outputs.create(&spec.topology).map_err(|source| ServiceError::Output {
        service: spec.name.clone(),
        dir: outputs.dir().to_owned(),
        source,
    })?;
    let path = file.path;
    // Never a pipe of the Daemon, which would die with it and take the Service down at its next write.
    let (stdout, stderr, writer) = match outputs.start_writer(&path) {
        Ok(writer) => (
            Stdio::from(writer.stdout),
            Stdio::from(writer.stderr),
            Some(writer.process),
        ),
        Err(error) => {
            eprintln!(
                "cannot start the writer of the output file {}, service {} appends to it directly, unbounded while no \
                 daemon runs: {error}",
                path.display(),
                spec.name
            );
            (Stdio::from(file.stdout), Stdio::from(file.stderr), None)
        }
    };
    let mut command = CommandWrap::with_new("/bin/sh", |command| {
        command
            .arg("-c")
            .arg(&spec.command)
            .current_dir(&spec.cwd)
            .env_clear()
            .envs(spec.env.iter().map(|(name, value)| (name, value)))
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(stderr);
    });
    process_group::lead_session(&mut command);
    let spawned = command.spawn();
    // The handles the process writes with are its own now, the Daemon keeps none.
    drop(command);
    let mut child = match spawned {
        Ok(child) => child,
        Err(source) => {
            let _ = std::fs::remove_file(&path);
            return Err(ServiceError::Spawn {
                service: spec.name.clone(),
                command: spec.command.clone(),
                cwd: spec.cwd.clone(),
                source,
            });
        }
    };
    let Some(pgid) = child.id().and_then(|pid| i32::try_from(pid).ok()).map(Pid::from_raw) else {
        // Unreachable on Unix, where every spawned child has a pid, but never leave a child unreaped.
        tokio::spawn(async move { Box::into_pin(child.kill()).await });
        let _ = std::fs::remove_file(&path);
        return Err(ServiceError::NoPid {
            service: spec.name.clone(),
        });
    };
    tree.begin(pgid);
    let tail = Tail::follow(path, file.reader, Arc::clone(output), writer);
    Ok(Process { child, pgid, tail })
}

/// Tells whoever follows the Service that its process is about to be restarted: one line on the Daemon's own
/// output, one in the Service's output, and the `Restarting` state.
fn announce_restart(service: &Service, spec: &ServiceSpec, why: &str, delay: Duration, restarts: u32) {
    let plan = format!(
        "{why}, restarting in {} (restart {restarts} of {})",
        crate::duration::format(delay),
        spec.restart.attempts
    );
    eprintln!("service {} of {} {plan}", spec.name, spec.worktree.display());
    output::lock(&service.output).push(format!("procly: the service {plan}\n").as_bytes());
    service.state.send_replace(ServiceState::Restarting);
}

/// Kills what is left of the tree of `process`, then reaps its leader, which ended: the one place a leader is reaped, so
/// that its pid, and the id of its session, stay its own while the tree is looked for. Returns how the leader ended.
async fn reap_once_decided(process: &mut Process, tree: &Tree) -> std::process::ExitStatus {
    kill_tree(tree).await;
    let status = process
        .child
        .wait()
        .await
        .expect("waiting fails only when the daemon lost track of its own child");
    tree.reaped();
    status
}

/// Watches the process of the Service until it ends for good: stopped, never healthy, or crashed more often than
/// its restart policy allows.
///
/// A process that crashes after the Service was healthy once is restarted, after a delay that doubles with each
/// crash in a row. One that dies before the Service was ever healthy is not: whoever asked for it is told at once
/// that it cannot start.
async fn supervise(
    service: Arc<Service>,
    spec: ServiceSpec,
    clock: Clock,
    records: Arc<ServiceRecords>,
    outputs: OutputFiles,
    first: Process,
) {
    let mut stop = service.stop.subscribe();
    let mut process = first;
    let mut ever_healthy = false;
    // How many restarts in a row were needed.
    let mut restarts = 0;
    loop {
        let started = clock.now();
        let monitor = tokio::spawn(monitor(
            Arc::clone(&service),
            Watch {
                label: format!("service {} of {}", spec.name, spec.worktree.display()),
                health: spec.health.clone(),
                health_timeout: spec.health_timeout,
                lease_ports: spec.lease_ports.clone(),
                idle: spec.idle,
            },
            clock.clone(),
        ));
        // The leader is waited for without being reaped: once it ends it stays a zombie that keeps its pid, and with it the
        // id of its session, so that no other process can take them while the tree is looked for and stopped. Nothing but
        // this function reaps it, and only after the tree is decided.
        let exited_by_itself = tokio::select! {
            () = process_group::ended(process.pgid) => true,
            _ = stop.wait_for(Option::is_some) => false,
        };
        monitor.abort();
        let healthy = *service.state.borrow() == ServiceState::Healthy;
        ever_healthy |= healthy;
        if !exited_by_itself {
            stop_tree(process.child.as_mut(), process.pgid, &service.tree).await;
        }
        // Processes of the tree still running once the leader has ended would keep holding the port.
        let status = reap_once_decided(&mut process, &service.tree).await;
        let ended = if exited_by_itself {
            process_group::service_end(status)
        } else {
            stop.borrow()
                .expect("the tree is stopped only once a stop was requested")
        };
        // The last lines tell why a Service died, they must be read before anyone is told it ended.
        process.tail.finish().await;

        let crashed = exited_by_itself && !service.is_stopping();
        if !(crashed && ever_healthy && spec.restart.attempts > 0) {
            finish(&service, &records, &spec.topology, &spec.name, ended).await;
            return;
        }
        if healthy && clock.now().saturating_sub(started) >= STABLE_RUN {
            restarts = 0;
        }
        let mut why = describe(ended);
        let next = loop {
            restarts += 1;
            if restarts > spec.restart.attempts {
                eprintln!(
                    "service {} of {} gave up after {} restarts, the last time it {why}",
                    spec.name,
                    spec.worktree.display(),
                    spec.restart.attempts
                );
                let end = ServiceEnd::Failed {
                    restarts: spec.restart.attempts,
                };
                finish(&service, &records, &spec.topology, &spec.name, end).await;
                return;
            }
            let delay = spec.restart.delay_before(restarts);
            // Read before the restart is announced, so that a clock moved by whoever reads the announcement is
            // already counted.
            let until = clock.now().saturating_add(delay);
            announce_restart(&service, &spec, &why, delay, restarts);
            let stopped = tokio::select! {
                () = clock.sleep_until(until) => false,
                _ = stop.wait_for(Option::is_some) => true,
            };
            if stopped {
                // A stop during the delay cancels the restart, there is no process left to stop.
                let end = stop
                    .borrow()
                    .expect("the stop was requested, which is what ended the wait");
                finish(&service, &records, &spec.topology, &spec.name, end).await;
                return;
            }
            match launch(&spec, &service.output, &outputs, &service.tree) {
                Ok(process) => {
                    record_restart(&records, &spec, &process, &service.tree).await;
                    break process;
                }
                Err(error) => {
                    let cause = std::error::Error::source(&error)
                        .map(ToString::to_string)
                        .unwrap_or_default();
                    eprintln!("{error}: {cause}");
                    why = format!("could not be started again: {cause}");
                }
            }
        };
        process = next;
        service.started_at.store(epoch_millis(&clock), Ordering::Relaxed);
        service.state.send_replace(ServiceState::Starting);
    }
}

/// The reading of `clock` as milliseconds since the Unix epoch, saturating far beyond any date a clock shows.
fn epoch_millis(clock: &Clock) -> u64 {
    u64::try_from(clock.since_epoch().as_millis()).unwrap_or(u64::MAX)
}

/// What a process did, for the Daemon log.
fn describe(end: ServiceEnd) -> String {
    match end {
        ServiceEnd::Exited { code } => format!("exited with code {code}"),
        ServiceEnd::Signaled { signal } => format!("was killed by signal {signal}"),
        ServiceEnd::HealthTimeout { .. }
        | ServiceEnd::Stopped
        | ServiceEnd::Idle { .. }
        | ServiceEnd::PortMismatch { .. }
        | ServiceEnd::Failed { .. }
        | ServiceEnd::Lost
        | ServiceEnd::Orphaned { .. }
        | ServiceEnd::NotListening { .. } => {
            unreachable!("only a process that exited by itself is described")
        }
    }
}

/// Watches the process group of a re-attached Service until it ends or is stopped. The Daemon cannot wait for a
/// process it did not start, so it looks at the leader instead, and it cannot restart the Service either, as the
/// environment it was started with is gone: the next `up` starts it again.
async fn supervise_reattached(
    service: Arc<Service>,
    record: ServiceRecord,
    tail: Option<Tail>,
    clock: Clock,
    records: Arc<ServiceRecords>,
) {
    let pgid = Pid::from_raw(record.pgid);
    let label = format!("service {} of {}", record.service, record.worktree.path.display());
    let idle = record.idle_ms.map(|idle_ms| {
        let (service, label, clock) = (Arc::clone(&service), label.clone(), clock.clone());
        let started = clock.now();
        tokio::spawn(async move {
            stop_when_idle(&service, &label, Duration::from_millis(idle_ms), &clock, started).await;
        })
    });
    let mut stop = service.stop.subscribe();
    let mut looks = 0_u32;
    let stopped = loop {
        let asked = tokio::select! {
            _ = stop.wait_for(Option::is_some) => true,
            () = tokio::time::sleep(LIVENESS_INTERVAL) => false,
        };
        if asked {
            break true;
        }
        if !process_group::is_alive(pgid) {
            break false;
        }
        looks = looks.wrapping_add(1);
        // A Service kept without a process list is judged as soon as one can be read, and a stop is asked if it is gone.
        if looks.is_multiple_of(PROVE_EVERY) {
            service.prove().await;
        }
    };
    if let Some(idle) = idle {
        idle.abort();
    }
    let end = if stopped {
        stop_detached(&service.tree).await;
        stop.borrow()
            .expect("the tree is stopped only once a stop was requested")
    } else {
        // Processes of the tree still running once the leader has ended would keep holding the port.
        kill_tree(&service.tree).await;
        eprintln!("{label} ended, its exit code is unknown, a previous Daemon started it");
        ServiceEnd::Lost
    };
    if let Some(tail) = tail {
        tail.finish().await;
    }
    finish(&service, &records, &record.topology(), &record.service, end).await;
}

/// What a Service found running after a restart of the Daemon does about its Lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LeaseHeld {
    /// Its tree accepts connections on the Lease and is what listens there.
    Yes,
    /// Nothing answers on the Lease, or what listens there is a Foreign process.
    No,
    /// Something answers on the Lease and the processes could not be listed to tell whether the tree is what listens:
    /// the listener may be a task in a group of its own, which only a listing finds.
    Unknown,
}

/// Whether the tree accepts connections on its Lease `port` and is what listens there, rather than a Foreign
/// process, so that a Service found running after a restart of the Daemon can be re-attached.
pub(super) async fn listens_on_lease(tree: &Tree, port: u16, lease_ports: &[u16]) -> LeaseHeld {
    let check = HealthCheck {
        port: Some(port),
        http: None,
        covered: Vec::new(),
    };
    if !check.passes().await {
        return LeaseHeld::No;
    }
    match ownership(tree, Some(port), lease_ports).await {
        Ownership::Listening => LeaseHeld::Yes,
        // The reading just taken decided what the tree is, unless the processes cannot be listed.
        _ if tree.is_unlisted() => LeaseHeld::Unknown,
        _ => LeaseHeld::No,
    }
}

/// What the task watching a run of the process needs to know about it.
struct Watch {
    /// Names the Service in the Daemon log.
    label: String,
    health: HealthCheck,
    health_timeout: Duration,
    /// The port of every Lease of the Service.
    lease_ports: Vec<u16>,
    idle: Option<Duration>,
}

/// Watches one run of the process: its health first, then its idleness.
async fn monitor(service: Arc<Service>, watch: Watch, clock: Clock) {
    let Some(healthy_at) = check_health(
        &service,
        &watch.health,
        &watch.lease_ports,
        &service.tree,
        watch.health_timeout,
        &clock,
    )
    .await
    else {
        return;
    };
    if let Some(timeout) = watch.idle {
        stop_when_idle(&service, &watch.label, timeout, &clock, healthy_at).await;
    }
}

/// Marks the Service healthy once its health check passes and its process tree listens on the Lease the check
/// probes, and stops it once `timeout` has passed without, or once it shows a Port mismatch. Returns the reading of
/// `clock` taken as the Service turned healthy, before anyone could hear of it, or `None` when it did not.
async fn check_health(
    service: &Service,
    check: &HealthCheck,
    lease_ports: &[u16],
    tree: &Tree,
    timeout: Duration,
    clock: &Clock,
) -> Option<Duration> {
    let mut state = service.state.subscribe();
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let mut wait = HEALTH_INTERVAL;
        if *state.borrow_and_update() != ServiceState::Starting {
            return None;
        }
        // A Service that accepts the connection and never answers must not hold the check past its deadline.
        if tokio::time::timeout_at(deadline, check.passes()).await == Ok(true) {
            // What answered may be a Foreign process holding the Lease while the Service listens elsewhere, or not yet.
            match ownership_all(tree, check, lease_ports).await {
                Ownership::Listening => {
                    let healthy_at = clock.now();
                    // A Service being stopped never turns healthy, `up` would answer with a Service about to vanish.
                    let healthy = service.state.send_if_modified(|state| {
                        let healthy = *state == ServiceState::Starting && !service.is_stopping();
                        if healthy {
                            *state = ServiceState::Healthy;
                        }
                        healthy
                    });
                    return healthy.then_some(healthy_at);
                }
                Ownership::Elsewhere(mismatch) => {
                    service.request_stop(mismatch);
                    return None;
                }
                Ownership::Pending => wait = OWNERSHIP_RETRY,
            }
        }
        if tokio::time::Instant::now() >= deadline {
            // Only now when nothing answers, a Service may open another port, such as a debugger, long before its Lease.
            let end = match ownership_all(tree, check, lease_ports).await {
                Ownership::Elsewhere(mismatch) => mismatch,
                Ownership::Listening | Ownership::Pending => ServiceEnd::HealthTimeout {
                    timeout_ms: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
                },
            };
            service.request_stop(end);
            return None;
        }
        tokio::select! {
            () = tokio::time::sleep_until(deadline.min(tokio::time::Instant::now() + wait)) => {}
            _ = state.changed() => {}
        }
    }
}

/// Stops the Service once it has printed nothing and used almost no CPU for `timeout`, counted from the reading
/// `started` of `clock`. Each look is dated when it begins, and the next one is due [`IDLE_INTERVAL`] later on the
/// clock, so that a clock that moves while `ps` answers brings the next look at once rather than putting it off.
async fn stop_when_idle(service: &Service, label: &str, timeout: Duration, clock: &Clock, started: Duration) {
    let origin = std::time::Instant::now();
    let mut tracker = None;
    let mut failed = false;
    loop {
        let now = clock.now();
        match look(service, now, origin).await {
            Ok(sample) => {
                let tracker = tracker.get_or_insert_with(|| IdleTracker::new(timeout, started, sample));
                // The first look too, which may come once the clock has passed the whole timeout.
                if tracker.is_idle(sample) {
                    eprintln!(
                        "{label} stopped after {} idle: it printed nothing and used under 1% of a CPU",
                        crate::duration::format(timeout)
                    );
                    service.request_stop(ServiceEnd::Idle {
                        timeout_ms: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
                    });
                    return;
                }
            }
            // Never stop a Service on a guess, and say so once rather than at every look.
            Err(error) if !failed => {
                failed = true;
                eprintln!("{label} cannot be checked for idleness, it is left running: cannot read CPU time: {error}");
            }
            Err(_) => {}
        }
        clock.sleep_until(now.saturating_add(IDLE_INTERVAL)).await;
    }
}

/// One look at the Service, begun at the reading `now` of the clock: when it last printed and the CPU time of its
/// whole tree, the tasks it started in groups of their own included.
async fn look(service: &Service, now: Duration, origin: std::time::Instant) -> io::Result<Sample> {
    let tree = service.tree.clone();
    let cpu = tokio::task::spawn_blocking(move || {
        tree.refresh_within(IDLE_SHARED);
        groups_cpu(&tree.groups(), IDLE_SHARED)
    })
    .await
    .map_err(io::Error::other)??;
    Ok(Sample {
        now,
        real: origin.elapsed(),
        printed_at: output::lock(&service.output).printed_at(),
        cpu,
    })
}

/// Whether the tree of a Service listens on the Lease its health check probes.
#[derive(Debug, PartialEq, Eq)]
enum Ownership {
    /// It listens on that Lease, or there is no Lease to probe, or its listeners cannot be read.
    Listening,
    /// It listens on another port than any of its Leases, and not on that one.
    Elsewhere(ServiceEnd),
    /// It listens on no port yet, or only on another of its Leases.
    Pending,
}

/// The verdict on every port the check probes, the first that is not `Listening` deciding: a Group listens on the
/// Lease of each Service it covers.
async fn ownership_all(tree: &Tree, check: &HealthCheck, lease_ports: &[u16]) -> Ownership {
    for port in check.ports() {
        match ownership(tree, Some(port), lease_ports).await {
            Ownership::Listening => {}
            other => return other,
        }
    }
    Ownership::Listening
}

async fn ownership(tree: &Tree, leased: Option<u16>, lease_ports: &[u16]) -> Ownership {
    let Some(leased) = leased else {
        return Ownership::Listening;
    };
    // The tasks of a tool such as turbo listen from groups of their own, found through their parents.
    let tree = tree.clone();
    let listening = tokio::task::spawn_blocking(move || {
        tree.refresh();
        listening_ports(&tree.groups())
    })
    .await
    .ok()
    .flatten();
    // Without `lsof`, the answer on the Lease is all there is to go by.
    listening.map_or(Ownership::Listening, |listening| {
        ownership_of(leased, lease_ports, &listening)
    })
}

/// Judges the ports a process group listens on against its Lease `leased`, a Port mismatch reporting the lowest
/// port it listens on that is none of its Leases.
fn ownership_of(leased: u16, lease_ports: &[u16], listening: &[u16]) -> Ownership {
    if listening.contains(&leased) {
        return Ownership::Listening;
    }
    match listening.iter().copied().find(|port| !lease_ports.contains(port)) {
        Some(actual) => Ownership::Elsewhere(ServiceEnd::PortMismatch { leased, actual }),
        None => Ownership::Pending,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Worktree;

    /// A Service of the Worktree `/p/repo` in `state`, with no process behind it.
    fn service_in(state: ServiceState) -> Arc<Service> {
        Arc::new(Service {
            port: None,
            topology: Topology {
                git_common_dir: PathBuf::from("/p/repo/.git"),
                worktree: Worktree {
                    path: PathBuf::from("/p/repo"),
                    slug: "main".to_owned(),
                    main: true,
                },
            },
            covers: Vec::new(),
            reattached: false,
            started_at: AtomicU64::new(0),
            output: Arc::new(Mutex::new(OutputBuffer::new(output::CAPACITY))),
            state: watch::Sender::new(state),
            stop: watch::Sender::new(None),
            tree: Tree::default(),
            unproven: tokio::sync::Mutex::default(),
        })
    }

    #[test]
    fn a_start_request_finds_the_service_under_its_key_while_it_runs_and_never_once_it_ended() {
        let key = |name: &str| (PathBuf::from("/p/repo"), name.to_owned());
        let running = service_in(ServiceState::Healthy);
        let table = HashMap::from([
            (key("web"), Arc::clone(&running)),
            (key("api"), service_in(ServiceState::Ended(ServiceEnd::Stopped))),
        ]);

        assert!(running_at(&table, &key("web")).is_some_and(|found| Arc::ptr_eq(&found, &running)));
        assert!(
            running_at(&table, &key("api")).is_none(),
            "an ended Service is replaced by a start"
        );
        assert!(running_at(&table, &key("db")).is_none());
    }

    #[test]
    fn listening_on_the_lease_is_owning_it_whatever_else_is_open() {
        assert_eq!(ownership_of(24000, &[24000], &[9229, 24000]), Ownership::Listening);
    }

    #[test]
    fn listening_elsewhere_than_any_lease_is_a_port_mismatch_on_the_lowest_such_port() {
        let mismatch = |actual| Ownership::Elsewhere(ServiceEnd::PortMismatch { leased: 24000, actual });
        assert_eq!(ownership_of(24000, &[24000, 24001], &[5173, 9229]), mismatch(5173));
        assert_eq!(ownership_of(24000, &[24000, 24001], &[24001, 5173]), mismatch(5173));
    }

    #[test]
    fn listening_on_nothing_or_only_on_another_lease_is_pending() {
        assert_eq!(ownership_of(24000, &[24000], &[]), Ownership::Pending);
        assert_eq!(ownership_of(24000, &[24000, 24001], &[24001]), Ownership::Pending);
    }
}
