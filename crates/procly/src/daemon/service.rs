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

use super::health::HealthCheck;
use super::listeners::listening_ports;
use super::process_group::{DRAIN_TIMEOUT, STOP_TIMEOUT, kill_group};
use crate::output::{self, OutputBuffer};
use crate::protocol::{ServiceEnd, StartOutcome};

/// How long the health check waits between 2 probes.
const HEALTH_INTERVAL: Duration = Duration::from_millis(50);

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
    /// The port of every Lease of the Service, which may listen on all of them.
    pub(crate) leases: Vec<u16>,
}

/// A Service runs at most once per Worktree: it is keyed by the root of its Worktree and its name.
type ServiceKey = (PathBuf, String);

/// Every Service the Daemon started, running or ended.
#[derive(Debug, Default)]
pub(crate) struct Services {
    table: Mutex<HashMap<ServiceKey, Arc<Service>>>,
    /// Held from the lookup to the insert, so that 2 concurrent requests never both start the Service.
    starting: Mutex<()>,
}

#[derive(Debug)]
pub(crate) struct Service {
    port: Option<u16>,
    /// The leader of the process group holding the Service's tree, so its pid is also the group id.
    pid: Pid,
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
    Ended(ServiceEnd),
}

impl Services {
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
        let service = spawn(spec()?)?;
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

    /// Waits until the Service is healthy, or returns how it ended before.
    pub(crate) async fn healthy(&self) -> Result<(), ServiceEnd> {
        let mut state = self.state.subscribe();
        let state = *state
            .wait_for(|state| *state != ServiceState::Starting)
            .await
            .expect("the service holds its own state sender, so the channel never closes");
        match state {
            ServiceState::Healthy => Ok(()),
            ServiceState::Ended(end) => Err(end),
            ServiceState::Starting => unreachable!("wait_for returned on a state other than Starting"),
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

fn spawn(spec: ServiceSpec) -> Result<Arc<Service>, ServiceError> {
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
    let Some(pid) = child.id().and_then(|pid| i32::try_from(pid).ok()).map(Pid::from_raw) else {
        // Unreachable on Unix, where every spawned child has a pid, but never leave a child unreaped.
        tokio::spawn(async move { Box::into_pin(child.kill()).await });
        return Err(ServiceError::NoPid { service: spec.name });
    };

    let output = Arc::new(Mutex::new(OutputBuffer::new(output::CAPACITY)));
    // Both pipes are drained continuously, a full pipe would block a chatty Service.
    let mut pumps = Vec::new();
    if let Some(stdout) = child.stdout().take() {
        pumps.push(tokio::spawn(pump(stdout, Arc::clone(&output))));
    }
    if let Some(stderr) = child.stderr().take() {
        pumps.push(tokio::spawn(pump(stderr, Arc::clone(&output))));
    }
    let service = Arc::new(Service {
        port: spec.health.port,
        pid,
        output,
        state: watch::Sender::new(ServiceState::Starting),
        stop: watch::Sender::new(None),
    });
    tokio::spawn(supervise(Arc::clone(&service), child, pumps));
    tokio::spawn(check_health(
        Arc::clone(&service),
        spec.health,
        spec.leases,
        spec.health_timeout,
    ));
    Ok(service)
}

async fn supervise(service: Arc<Service>, mut child: Box<dyn ChildWrapper>, pumps: Vec<JoinHandle<()>>) {
    let mut stop = service.stop.subscribe();
    let exited = tokio::select! {
        status = child.wait() => Some(status),
        _ = stop.wait_for(Option::is_some) => None,
    };
    let end = if let Some(status) = exited {
        end_of(status.expect("waiting fails only when the daemon lost track of its own child"))
    } else {
        stop_tree(child.as_mut()).await;
        stop.borrow()
            .expect("the tree is stopped only once a stop was requested")
    };
    // Members of the group still running once the leader has ended would keep holding the port.
    let pgid = service.pid;
    let _ = tokio::task::spawn_blocking(move || kill_group(pgid)).await;
    // The last lines tell why a Service died, they must be read before anyone is told it ended.
    let _ = tokio::time::timeout(DRAIN_TIMEOUT, join_all(pumps)).await;
    service.state.send_replace(ServiceState::Ended(end));
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

/// Marks the Service healthy once its health check passes, and stops it once `timeout` has passed without, or once
/// it shows a Port mismatch.
async fn check_health(service: Arc<Service>, check: HealthCheck, leases: Vec<u16>, timeout: Duration) {
    let mut state = service.state.subscribe();
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if *state.borrow_and_update() != ServiceState::Starting {
            return;
        }
        // A Service that accepts the connection and never answers must not hold the check past its deadline.
        if tokio::time::timeout_at(deadline, check.passes()).await == Ok(true) {
            // What answered may be a Foreign process holding the Lease while the Service listens elsewhere.
            if let Some(mismatch) = port_mismatch(&service, &check, &leases).await {
                service.request_stop(mismatch);
                return;
            }
            // A Service being stopped never turns healthy, `up` would answer with a Service about to vanish.
            service.state.send_if_modified(|state| {
                let healthy = *state == ServiceState::Starting && !service.is_stopping();
                if healthy {
                    *state = ServiceState::Healthy;
                }
                healthy
            });
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            // Only now, a Service may open another port, such as a debugger, long before the one of its Lease.
            let end = port_mismatch(&service, &check, &leases)
                .await
                .unwrap_or(ServiceEnd::HealthTimeout {
                    timeout_ms: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
                });
            service.request_stop(end);
            return;
        }
        tokio::select! {
            () = tokio::time::sleep_until(deadline.min(tokio::time::Instant::now() + HEALTH_INTERVAL)) => {}
            _ = state.changed() => {}
        }
    }
}

/// The Port mismatch of the Service, when its process group listens on the port of none of its Leases and not on
/// the one its health check probes. `None` as long as its listeners cannot tell otherwise.
async fn port_mismatch(service: &Service, check: &HealthCheck, lease_ports: &[u16]) -> Option<ServiceEnd> {
    let leased = check.port?;
    let pgid = service.pid;
    let listening = tokio::task::spawn_blocking(move || listening_ports(pgid))
        .await
        .ok()??;
    mismatched_port(leased, lease_ports, &listening).map(|actual| ServiceEnd::PortMismatch { leased, actual })
}

/// The port the Service listens on instead of its Lease `leased`, the lowest one when it listens on several.
///
/// A Service listening on no port at all tells nothing, it may run a container publishing the port from another
/// process, so it is no Port mismatch.
fn mismatched_port(leased: u16, lease_ports: &[u16], listening: &[u16]) -> Option<u16> {
    if listening.contains(&leased) {
        return None;
    }
    listening.iter().copied().find(|port| !lease_ports.contains(port))
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
    fn listening_on_the_lease_is_no_port_mismatch_whatever_else_is_open() {
        assert_eq!(mismatched_port(24000, &[24000], &[9229, 24000]), None);
    }

    #[test]
    fn listening_elsewhere_than_any_lease_is_a_port_mismatch_on_the_lowest_such_port() {
        assert_eq!(mismatched_port(24000, &[24000, 24001], &[5173, 9229]), Some(5173));
        assert_eq!(mismatched_port(24000, &[24000, 24001], &[24001, 5173]), Some(5173));
    }

    #[test]
    fn listening_on_nothing_or_only_on_another_lease_is_no_port_mismatch() {
        assert_eq!(mismatched_port(24000, &[24000], &[]), None);
        assert_eq!(mismatched_port(24000, &[24000, 24001], &[24001]), None);
    }
}
