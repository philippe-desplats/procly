//! Services: declared Process services, run with pipes in a process group of their own and health-checked.

use std::collections::HashMap;
use std::io;
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nix::unistd::Pid;
use process_wrap::tokio::{ChildWrapper, CommandWrap, ProcessGroup};
use tokio::io::{AsyncRead, AsyncReadExt as _};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use super::clock::Clock;
use super::cpu::group_cpu;
use super::health::HealthCheck;
use super::idle::{IdleTracker, Sample};
use super::listeners::listening_ports;
use super::process_group::{self, DRAIN_TIMEOUT, STOP_TIMEOUT, kill_group};
use super::record::{ServiceRecord, ServiceRecords};
use crate::config::RestartPolicy;
use crate::output::{self, OutputBuffer};
use crate::protocol::{self, ServiceEnd, ServiceStatus, StartOutcome};
use crate::topology::Topology;

/// How long the health check waits between 2 probes.
const HEALTH_INTERVAL: Duration = Duration::from_millis(50);
/// How often the Daemon looks at a Service to tell whether it is idle.
const IDLE_INTERVAL: Duration = Duration::from_secs(10);
/// How often the Daemon looks whether the process of a re-attached Service, which is not its child, still runs.
const LIVENESS_INTERVAL: Duration = Duration::from_millis(100);
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
    #[error("service {service} started without a process id")]
    NoPid { service: String },
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
}

/// A Service runs at most once per Worktree: it is keyed by the root of its Worktree and its name.
type ServiceKey = (PathBuf, String);

/// Every Service the Daemon started, running or ended.
#[derive(Debug)]
pub(crate) struct Services {
    clock: Clock,
    records: Arc<ServiceRecords>,
    table: Mutex<HashMap<ServiceKey, Arc<Service>>>,
    /// Held from the lookup to the insert, so that 2 concurrent requests never both start the Service.
    starting: Mutex<()>,
}

#[derive(Debug)]
pub(crate) struct Service {
    port: Option<u16>,
    topology: Topology,
    /// Whether a previous Daemon started the Service, and this one found it running when it started.
    reattached: bool,
    /// The output of every run of the Service, restarts included.
    output: Arc<Mutex<OutputBuffer>>,
    state: watch::Sender<ServiceState>,
    /// Set once to ask the task watching the process to stop its tree, with the end to report once it is gone.
    stop: watch::Sender<Option<ServiceEnd>>,
}

/// The outcome of a start request.
#[derive(Debug)]
pub(crate) enum Start {
    /// The Service was started, or found running, and its health is pending or known.
    Ready {
        service: Arc<Service>,
        outcome: StartOutcome,
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
    pub(crate) fn new(clock: Clock, records: ServiceRecords) -> Self {
        Self {
            clock,
            records: Arc::new(records),
            table: Mutex::default(),
            starting: Mutex::default(),
        }
    }

    /// Returns the running Service `name` of `worktree`, or spawns it from the spec built by `spec`, without
    /// waiting for it to be healthy. A Service being stopped is no Reuse, it still holds its port until it ends.
    /// Must run inside the Tokio runtime.
    pub(crate) fn start<E: From<ServiceError>>(
        &self,
        worktree: &Path,
        name: &str,
        spec: impl FnOnce() -> Result<ServiceSpec, E>,
    ) -> Result<Start, E> {
        // Held through the configuration read and the spawn, on a blocking thread and never across an await.
        let _starting = self
            .starting
            .lock()
            .expect("the start mutex is never poisoned, no code panics while holding it");
        let key = (worktree.to_owned(), name.to_owned());
        if let Some(service) = self.table().get(&key).filter(|service| service.is_running()) {
            let service = Arc::clone(service);
            return Ok(if service.is_stopping() {
                Start::Stopping(service)
            } else {
                Start::Ready {
                    service,
                    outcome: StartOutcome::Reused,
                }
            });
        }
        let service = spawn(spec()?, self.clock.clone(), Arc::clone(&self.records))?;
        self.table().insert(key, Arc::clone(&service));
        Ok(Start::Ready {
            service,
            outcome: StartOutcome::Started,
        })
    }

    /// Asks every running Service of `worktree`, or only the one named `name`, to stop, and returns them sorted by
    /// name. Wait for each with [`Service::ended`].
    pub(crate) fn stop(&self, worktree: &Path, name: Option<&str>) -> Vec<(String, Arc<Service>)> {
        let mut stopping: Vec<_> = self
            .table()
            .iter()
            .filter(|((path, service_name), service)| {
                path == worktree && name.is_none_or(|name| name == service_name) && service.is_running()
            })
            .map(|((_, service_name), service)| (service_name.clone(), Arc::clone(service)))
            .collect();
        stopping.sort_by(|(a, _), (b, _)| a.cmp(b));
        for (_, service) in &stopping {
            service.request_stop(ServiceEnd::Stopped);
        }
        stopping
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

    /// The records of the running Services, which a new Daemon reconciles before it starts any.
    pub(crate) fn records(&self) -> Arc<ServiceRecords> {
        Arc::clone(&self.records)
    }

    /// Takes over the Service of `record`, which a previous Daemon started and which still runs and listens on its
    /// Lease: it is healthy, and supervised through its process group, as it is no child of this Daemon. Its output
    /// went to the previous Daemon and is lost. Must run inside the Tokio runtime.
    pub(crate) fn reattach(&self, record: ServiceRecord) {
        let service = Arc::new(Service {
            port: record.port,
            topology: record.topology(),
            reattached: true,
            output: Arc::new(Mutex::new(OutputBuffer::new(output::CAPACITY))),
            state: watch::Sender::new(ServiceState::Healthy),
            stop: watch::Sender::new(None),
        });
        self.table().insert(
            (record.worktree.path.clone(), record.service.clone()),
            Arc::clone(&service),
        );
        tokio::spawn(supervise_reattached(
            service,
            record,
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
            reattached: false,
            output: Arc::new(Mutex::new(OutputBuffer::new(output::CAPACITY))),
            state: watch::Sender::new(ServiceState::Ended(end)),
            stop: watch::Sender::new(None),
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
    pumps: Vec<JoinHandle<()>>,
}

fn spawn(spec: ServiceSpec, clock: Clock, records: Arc<ServiceRecords>) -> Result<Arc<Service>, ServiceError> {
    let output = Arc::new(Mutex::new(OutputBuffer::new(output::CAPACITY)));
    let process = launch(&spec, &output)?;
    // Recorded before anyone hears of the Service, so that a crash of the Daemon from now on leaves a trace of it.
    record_run(&records, record_of(&spec), process.pgid);
    let service = Arc::new(Service {
        port: spec.health.port,
        topology: spec.topology.clone(),
        reattached: false,
        output,
        state: watch::Sender::new(ServiceState::Starting),
        stop: watch::Sender::new(None),
    });
    tokio::spawn(supervise(Arc::clone(&service), spec, clock, records, process));
    Ok(service)
}

/// The record of a Service, waiting for the process group of its current run.
fn record_of(spec: &ServiceSpec) -> ServiceRecord {
    ServiceRecord {
        repository: spec.topology.git_common_dir.clone(),
        worktree: spec.topology.worktree.clone(),
        service: spec.name.clone(),
        pgid: 0,
        started: None,
        port: spec.health.port,
        lease_ports: spec.lease_ports.clone(),
        idle_ms: spec
            .idle
            .map(|idle| u64::try_from(idle.as_millis()).unwrap_or(u64::MAX)),
    }
}

/// Records the run of the process group `pgid` in `record`. Blocking: it reads when the leader started with `ps`
/// and writes the document.
fn record_run(records: &ServiceRecords, mut record: ServiceRecord, pgid: Pid) {
    record.pgid = pgid.as_raw();
    record.started = process_group::identity(pgid).map(|identity| identity.started);
    records.put(record);
}

/// Ends the Service with `end`, once it is no longer recorded, so that a Daemon starting right after never finds a
/// Service that ended.
async fn finish(service: &Service, records: &Arc<ServiceRecords>, topology: &Topology, name: &str, end: ServiceEnd) {
    let (records, worktree, name) = (Arc::clone(records), topology.worktree.path.clone(), name.to_owned());
    // Writing the document syncs it to disk, which must not park a runtime worker.
    let _ = tokio::task::spawn_blocking(move || records.remove(worktree, name)).await;
    service.state.send_replace(ServiceState::Ended(end));
}

/// Starts the process of the Service, writing its output into `output`.
fn launch(spec: &ServiceSpec, output: &Arc<Mutex<OutputBuffer>>) -> Result<Process, ServiceError> {
    let mut command = CommandWrap::with_new("/bin/sh", |command| {
        command
            .arg("-c")
            .arg(&spec.command)
            .current_dir(&spec.cwd)
            .env_clear()
            .envs(spec.env.iter().map(|(name, value)| (name, value)))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
    });
    command.wrap(ProcessGroup::leader());
    let mut child = command.spawn().map_err(|source| ServiceError::Spawn {
        service: spec.name.clone(),
        command: spec.command.clone(),
        cwd: spec.cwd.clone(),
        source,
    })?;
    let Some(pgid) = child.id().and_then(|pid| i32::try_from(pid).ok()).map(Pid::from_raw) else {
        // Unreachable on Unix, where every spawned child has a pid, but never leave a child unreaped.
        tokio::spawn(async move { Box::into_pin(child.kill()).await });
        return Err(ServiceError::NoPid {
            service: spec.name.clone(),
        });
    };

    // Both pipes are drained continuously, a full pipe would block a chatty Service.
    let mut pumps = Vec::new();
    if let Some(stdout) = child.stdout().take() {
        pumps.push(tokio::spawn(pump(stdout, Arc::clone(output))));
    }
    if let Some(stderr) = child.stderr().take() {
        pumps.push(tokio::spawn(pump(stderr, Arc::clone(output))));
    }
    Ok(Process { child, pgid, pumps })
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
                pgid: process.pgid,
            },
            clock.clone(),
        ));
        let exited = tokio::select! {
            status = process.child.wait() => Some(status),
            _ = stop.wait_for(Option::is_some) => None,
        };
        monitor.abort();
        let healthy = *service.state.borrow() == ServiceState::Healthy;
        ever_healthy |= healthy;
        let exited_by_itself = exited.is_some();
        let ended = if let Some(status) = exited {
            end_of(status.expect("waiting fails only when the daemon lost track of its own child"))
        } else {
            stop_tree(process.child.as_mut()).await;
            stop.borrow()
                .expect("the tree is stopped only once a stop was requested")
        };
        // Members of the group still running once the leader has ended would keep holding the port.
        let pgid = process.pgid;
        let _ = tokio::task::spawn_blocking(move || kill_group(pgid)).await;
        // The last lines tell why a Service died, they must be read before anyone is told it ended.
        let _ = tokio::time::timeout(DRAIN_TIMEOUT, join_all(process.pumps)).await;

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
            let plan = format!(
                "{why}, restarting in {} (restart {restarts} of {})",
                crate::duration::format(delay),
                spec.restart.attempts
            );
            eprintln!("service {} of {} {plan}", spec.name, spec.worktree.display());
            output::lock(&service.output).push(format!("procly: the service {plan}\n").as_bytes());
            service.state.send_replace(ServiceState::Restarting);
            let stopped = tokio::select! {
                () = clock.sleep(delay) => false,
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
            match launch(&spec, &service.output) {
                Ok(process) => {
                    let (records, record, pgid) = (Arc::clone(&records), record_of(&spec), process.pgid);
                    let _ = tokio::task::spawn_blocking(move || record_run(&records, record, pgid)).await;
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
        service.state.send_replace(ServiceState::Starting);
    }
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

/// Stops the leader of the process group: `SIGTERM` to the whole group, then `SIGKILL` after the stop timeout.
async fn stop_tree(child: &mut dyn ChildWrapper) {
    if let Err(error) = child.signal(nix::libc::SIGTERM) {
        eprintln!("cannot send SIGTERM to process group {:?}: {error}", child.id());
    }
    if tokio::time::timeout(STOP_TIMEOUT, child.wait()).await.is_err() {
        if let Err(error) = child.start_kill() {
            eprintln!("cannot kill process group {:?}: {error}", child.id());
        }
        let _ = child.wait().await;
    }
}

/// Watches the process group of a re-attached Service until it ends or is stopped. The Daemon cannot wait for a
/// process it did not start, so it looks at the leader instead, and it cannot restart the Service either, as the
/// environment it was started with is gone: the next `up` starts it again.
async fn supervise_reattached(
    service: Arc<Service>,
    record: ServiceRecord,
    clock: Clock,
    records: Arc<ServiceRecords>,
) {
    let pgid = Pid::from_raw(record.pgid);
    let label = format!("service {} of {}", record.service, record.worktree.path.display());
    let idle = record.idle_ms.map(|idle_ms| {
        let (service, label, clock) = (Arc::clone(&service), label.clone(), clock.clone());
        tokio::spawn(
            async move { stop_when_idle(&service, &label, pgid, Duration::from_millis(idle_ms), &clock).await },
        )
    });
    let mut stop = service.stop.subscribe();
    let stopped = loop {
        tokio::select! {
            _ = stop.wait_for(Option::is_some) => break true,
            () = tokio::time::sleep(LIVENESS_INTERVAL) => if !process_group::is_alive(pgid) {
                break false;
            },
        }
    };
    if let Some(idle) = idle {
        idle.abort();
    }
    let end = if stopped {
        process_group::stop_group(pgid).await;
        stop.borrow()
            .expect("the group is stopped only once a stop was requested")
    } else {
        // Members of the group still running once the leader has ended would keep holding the port.
        let _ = tokio::task::spawn_blocking(move || kill_group(pgid)).await;
        eprintln!("{label} ended, its exit code is unknown, a previous Daemon started it");
        ServiceEnd::Lost
    };
    finish(&service, &records, &record.topology(), &record.service, end).await;
}

/// Whether the process group `pgid` accepts connections on its Lease `port` and is what listens there, rather than
/// a Foreign process, so that a Service found running after a restart of the Daemon can be re-attached.
pub(super) async fn listens_on_lease(pgid: Pid, port: u16, lease_ports: &[u16]) -> bool {
    let check = HealthCheck {
        port: Some(port),
        http: None,
    };
    check.passes().await && ownership(pgid, Some(port), lease_ports).await == Ownership::Listening
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
    pgid: Pid,
}

/// Watches one run of the process: its health first, then its idleness.
async fn monitor(service: Arc<Service>, watch: Watch, clock: Clock) {
    if !check_health(
        &service,
        &watch.health,
        &watch.lease_ports,
        watch.pgid,
        watch.health_timeout,
    )
    .await
    {
        return;
    }
    if let Some(timeout) = watch.idle {
        stop_when_idle(&service, &watch.label, watch.pgid, timeout, &clock).await;
    }
}

/// Marks the Service healthy once its health check passes and its process group listens on the Lease the check
/// probes, and stops it once `timeout` has passed without, or once it shows a Port mismatch. Returns whether the
/// Service turned healthy.
async fn check_health(
    service: &Service,
    check: &HealthCheck,
    lease_ports: &[u16],
    pgid: Pid,
    timeout: Duration,
) -> bool {
    let mut state = service.state.subscribe();
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if *state.borrow_and_update() != ServiceState::Starting {
            return false;
        }
        // A Service that accepts the connection and never answers must not hold the check past its deadline.
        if tokio::time::timeout_at(deadline, check.passes()).await == Ok(true) {
            // What answered may be a Foreign process holding the Lease while the Service listens elsewhere, or not yet.
            match ownership(pgid, check.port, lease_ports).await {
                Ownership::Listening => {
                    // A Service being stopped never turns healthy, `up` would answer with a Service about to vanish.
                    return service.state.send_if_modified(|state| {
                        let healthy = *state == ServiceState::Starting && !service.is_stopping();
                        if healthy {
                            *state = ServiceState::Healthy;
                        }
                        healthy
                    });
                }
                Ownership::Elsewhere(mismatch) => {
                    service.request_stop(mismatch);
                    return false;
                }
                Ownership::Pending => {}
            }
        }
        if tokio::time::Instant::now() >= deadline {
            // Only now when nothing answers, a Service may open another port, such as a debugger, long before its Lease.
            let end = match ownership(pgid, check.port, lease_ports).await {
                Ownership::Elsewhere(mismatch) => mismatch,
                Ownership::Listening | Ownership::Pending => ServiceEnd::HealthTimeout {
                    timeout_ms: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
                },
            };
            service.request_stop(end);
            return false;
        }
        tokio::select! {
            () = tokio::time::sleep_until(deadline.min(tokio::time::Instant::now() + HEALTH_INTERVAL)) => {}
            _ = state.changed() => {}
        }
    }
}

/// Stops the Service once it has printed nothing and used almost no CPU for `timeout`.
async fn stop_when_idle(service: &Service, label: &str, pgid: Pid, timeout: Duration, clock: &Clock) {
    let origin = std::time::Instant::now();
    let mut tracker = None;
    let mut failed = false;
    loop {
        match look(service, pgid, clock, origin).await {
            Ok(sample) => {
                let tracker = tracker.get_or_insert_with(|| IdleTracker::new(timeout, sample));
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
        clock.sleep(IDLE_INTERVAL).await;
    }
}

/// One look at the Service: what it printed and the CPU time of its tree.
async fn look(service: &Service, pgid: Pid, clock: &Clock, origin: std::time::Instant) -> io::Result<Sample> {
    let cpu = tokio::task::spawn_blocking(move || group_cpu(pgid))
        .await
        .map_err(io::Error::other)??;
    Ok(Sample {
        now: clock.now(),
        real: origin.elapsed(),
        written: output::lock(&service.output).written(),
        cpu,
    })
}

/// Whether the process group of a Service listens on the Lease its health check probes.
#[derive(Debug, PartialEq, Eq)]
enum Ownership {
    /// It listens on that Lease, or there is no Lease to probe, or its listeners cannot be read.
    Listening,
    /// It listens on another port than any of its Leases, and not on that one.
    Elsewhere(ServiceEnd),
    /// It listens on no port yet, or only on another of its Leases.
    Pending,
}

async fn ownership(pgid: Pid, leased: Option<u16>, lease_ports: &[u16]) -> Ownership {
    let Some(leased) = leased else {
        return Ownership::Listening;
    };
    let listening = tokio::task::spawn_blocking(move || listening_ports(pgid))
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

fn end_of(status: ExitStatus) -> ServiceEnd {
    match status.signal() {
        Some(signal) => ServiceEnd::Signaled { signal },
        None => ServiceEnd::Exited {
            code: status.code().expect("an exit status without a signal carries a code"),
        },
    }
}

async fn join_all(pumps: Vec<JoinHandle<()>>) {
    for pump in pumps {
        let _ = pump.await;
    }
}

async fn pump(mut reader: impl AsyncRead + Unpin, output: Arc<Mutex<OutputBuffer>>) {
    let mut chunk = [0; 8192];
    // An error reading a pipe means the Service closed it, which is the end of its output.
    while let Ok(read @ 1..) = reader.read(&mut chunk).await {
        output::lock(&output).push(&chunk[..read]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
