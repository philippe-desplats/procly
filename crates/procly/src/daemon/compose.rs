//! Compose services in Shared mode: one container of the Repository's compose file serves every Worktree.
//!
//! Procly drives the compose file only through the `docker compose` v2 CLI and never rewrites it (ADR 0004). The
//! container is created from the compose file of the Main worktree under one project name, so that every Worktree
//! finds the same container. Worktrees are counted, and once the last one let go the container is stopped, never
//! removed, after its Grace period.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde::Deserialize;
use tokio::process::Command;
use tokio::task::JoinSet;

use super::clock::Clock;
use super::health::HealthCheck;
use crate::protocol::{Released, ServiceEnd, StartOutcome};

/// Where to install `docker compose` v2, named in the error of a machine that lacks it.
const INSTALL_URL: &str = "https://docs.docker.com/compose/install/";
/// How long the health check waits between 2 probes.
const HEALTH_INTERVAL: Duration = Duration::from_millis(50);
/// How many lines of the logs of a container that never became healthy `up` shows.
const LOG_LINES: &str = "20";

#[derive(Debug, thiserror::Error)]
pub(crate) enum ComposeError {
    #[error("docker compose v2 is required by service {service}: {reason}, install it from {INSTALL_URL}")]
    Unavailable { service: String, reason: String },
    #[error("`docker compose {args}` failed for service {service}: {stderr}")]
    Command {
        service: String,
        args: String,
        stderr: String,
    },
    #[error("`docker compose config` printed a stack Procly cannot read for service {service}")]
    Config {
        service: String,
        #[source]
        source: serde_json::Error,
    },
    #[error(
        "service {service} maps to the compose service {compose}, but there is no service {compose} in the compose file of {dir}"
    )]
    UnknownService {
        service: String,
        compose: String,
        dir: PathBuf,
    },
    #[error(
        "the compose service {compose} of service {service} does not publish port {port}, the Lease of Port name \
         {port_name}: have its `ports` interpolate one of {variables}, such as \"${{PORT_{upper}:-{port}}}:<container \
         port>\""
    )]
    NotPublished {
        service: String,
        compose: String,
        port_name: String,
        port: u16,
        upper: String,
        variables: String,
    },
    #[error(transparent)]
    Prepare(#[from] anyhow::Error),
}

/// What it takes to start the container of one Compose service.
#[derive(Debug)]
pub(crate) struct ComposeSpec {
    /// The name of the Service in `procly.toml`, named in errors.
    pub(crate) name: String,
    /// The service of the compose file the Service maps to.
    pub(crate) compose: String,
    /// The Main worktree, where the compose file is read from.
    pub(crate) project_dir: PathBuf,
    /// The environment of the caller, then the Leases and the variables the compose file interpolates.
    pub(crate) env: Vec<(String, String)>,
    /// The Leases of the Service, which the compose file must publish.
    pub(crate) published: Vec<Published>,
    /// The names of the variables the compose file may interpolate the Leases from.
    pub(crate) variables: Vec<String>,
    pub(crate) health: HealthCheck,
    pub(crate) health_timeout: Duration,
}

/// A Lease the compose file must publish on the host.
#[derive(Debug)]
pub(crate) struct Published {
    pub(crate) port_name: String,
    pub(crate) port: u16,
}

/// What `up` found out about a container.
#[derive(Debug)]
pub(crate) struct Brought {
    pub(crate) outcome: StartOutcome,
    pub(crate) port: Option<u16>,
    /// `Err` when the container never answered its health check, carrying its recent logs.
    pub(crate) health: Result<(), (ServiceEnd, String)>,
}

/// A Compose service is shared by the Worktrees of one Repository, so it is keyed by the Repository and its name.
pub(crate) type ComposeKey = (PathBuf, String);

/// Every container Procly started from a compose file.
#[derive(Debug)]
pub(crate) struct Composes {
    clock: Clock,
    table: Mutex<HashMap<ComposeKey, Arc<Container>>>,
}

#[derive(Debug, Default)]
struct Container {
    /// Held through every `docker compose` command on the container, so that a start and a stop never overlap.
    commands: tokio::sync::Mutex<()>,
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    /// `None` when the container was never started by Procly or has been stopped.
    alive: Option<Alive>,
    /// The Worktrees that need the container.
    worktrees: BTreeSet<PathBuf>,
    /// Bumped whenever a Worktree starts needing the container, so that a stop waiting out its Grace period knows it
    /// was overtaken.
    generation: u64,
}

#[derive(Debug, Clone)]
struct Alive {
    docker: Docker,
    port: Option<u16>,
    grace: Duration,
}

/// Where and how `docker compose` is run for one project.
#[derive(Debug, Clone)]
struct Docker {
    service: String,
    compose: String,
    project_dir: PathBuf,
    project: String,
    env: Vec<(String, String)>,
}

impl Composes {
    pub(crate) fn new(clock: Clock) -> Self {
        Self {
            clock,
            table: Mutex::default(),
        }
    }

    /// Makes sure the container of the Compose service `key` runs for `worktree`, and waits until it is healthy.
    ///
    /// A container that Procly started and that still runs is a Reuse, whatever the Worktree. Otherwise `prepare`
    /// builds what it takes to start it, on a blocking thread.
    pub(crate) async fn up<P>(
        &self,
        key: ComposeKey,
        worktree: PathBuf,
        grace: Duration,
        prepare: P,
    ) -> Result<Brought, ComposeError>
    where
        P: FnOnce() -> anyhow::Result<ComposeSpec> + Send + 'static,
    {
        let container = self.container(&key);
        let _commands = container.commands.lock().await;
        let alive = container.state().alive.clone();
        // A container stopped from outside Procly is started again below, for the Worktrees that already needed it too.
        if let Some(mut alive) = alive
            && alive.docker.is_running().await
        {
            alive.grace = grace;
            let port = alive.port;
            let mut state = container.state();
            state.alive = Some(alive);
            state.worktrees.insert(worktree);
            state.generation += 1;
            return Ok(Brought {
                outcome: StartOutcome::Reused,
                port,
                health: Ok(()),
            });
        }

        let spec = tokio::task::spawn_blocking(prepare)
            .await
            .map_err(|error| anyhow::anyhow!("the service could not be started: {error}"))??;
        let port = spec.health.port;
        let began = Instant::now();
        let (docker, waited_out) = start(&spec).await?;
        let health = match waited_out {
            Some(end) => Err(end),
            // The health timeout covers `docker compose up --wait` and the probes together.
            None => wait_healthy(&spec.health, spec.health_timeout.saturating_sub(began.elapsed())).await,
        };
        if let Err(end) = health {
            let logs = docker.logs().await;
            // Stopped, never removed: the data stays, and the next `up` starts it again.
            let _ = docker.stop().await;
            return Ok(Brought {
                outcome: StartOutcome::Started,
                port,
                health: Err((end, logs)),
            });
        }
        let mut state = container.state();
        state.alive = Some(Alive { docker, port, grace });
        state.worktrees.insert(worktree);
        state.generation += 1;
        Ok(Brought {
            outcome: StartOutcome::Started,
            port,
            health: Ok(()),
        })
    }

    /// Lets `worktree` go of the containers of the Repository `repository`, of the Compose service `name` only when
    /// given. A container no Worktree needs anymore stops once its Grace period has passed on the clock.
    pub(crate) fn release(&self, repository: &Path, worktree: &Path, name: Option<&str>) -> Vec<Released> {
        let mut released = Vec::new();
        let containers: Vec<_> = self
            .table()
            .iter()
            .filter(|((path, service), _)| path == repository && name.is_none_or(|name| name == service))
            .map(|((_, service), container)| (service.clone(), Arc::clone(container)))
            .collect();
        for (service, container) in containers {
            let mut state = container.state();
            let Some(grace) = state.alive.as_ref().map(|alive| alive.grace) else {
                continue;
            };
            if !state.worktrees.remove(worktree) {
                continue;
            }
            let others = state.worktrees.len();
            if others == 0 {
                state.generation += 1;
                tokio::spawn(stop_after(
                    self.clock.clone(),
                    Arc::clone(&container),
                    grace,
                    state.generation,
                ));
            }
            released.push(Released {
                service,
                others,
                grace_ms: u64::try_from(grace.as_millis()).unwrap_or(u64::MAX),
            });
        }
        released.sort_by(|a, b| a.service.cmp(&b.service));
        released
    }

    /// Stops every container Procly started, without waiting for any Grace period, for a Daemon about to exit.
    pub(crate) async fn stop_all(&self) {
        let containers: Vec<_> = self.table().values().cloned().collect();
        let mut stopping = JoinSet::new();
        for container in containers {
            stopping.spawn(async move {
                let _commands = container.commands.lock().await;
                let alive = {
                    let mut state = container.state();
                    state.worktrees.clear();
                    state.generation += 1;
                    state.alive.take()
                };
                let Some(alive) = alive else { return };
                if let Err(error) = alive.docker.stop().await {
                    eprintln!("cannot stop the container of service {}: {error}", alive.docker.service);
                }
            });
        }
        while stopping.join_next().await.is_some() {}
    }

    fn container(&self, key: &ComposeKey) -> Arc<Container> {
        Arc::clone(self.table().entry(key.clone()).or_default())
    }

    fn table(&self) -> MutexGuard<'_, HashMap<ComposeKey, Arc<Container>>> {
        self.table
            .lock()
            .expect("the compose table mutex is never poisoned, no code panics while holding it")
    }
}

impl Container {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .expect("the container state mutex is never poisoned, no code panics while holding it")
    }
}

/// Stops the container after `grace`, unless a Worktree needed it again in the meantime.
async fn stop_after(clock: Clock, container: Arc<Container>, grace: Duration, generation: u64) {
    clock.sleep(grace).await;
    let _commands = container.commands.lock().await;
    let alive = {
        let state = container.state();
        if state.generation != generation || !state.worktrees.is_empty() {
            return;
        }
        state.alive.clone()
    };
    let Some(alive) = alive else { return };
    match alive.docker.stop().await {
        Ok(()) => {
            container.state().alive = None;
            eprintln!(
                "stopped the container of service {}, no Worktree needs it",
                alive.docker.service
            );
        }
        // Still counted as running, so that the next `up` finds it and the next release schedules another stop.
        Err(error) => eprintln!("cannot stop the container of service {}: {error}", alive.docker.service),
    }
}

/// Starts the container of the spec and returns the commands that drive it afterwards, with the end of the wait
/// when the container was still not running, or not healthy by its own health check, after the health timeout.
async fn start(spec: &ComposeSpec) -> Result<(Docker, Option<ServiceEnd>), ComposeError> {
    let mut docker = Docker {
        service: spec.name.clone(),
        compose: spec.compose.clone(),
        project_dir: spec.project_dir.clone(),
        project: String::new(),
        env: spec.env.clone(),
    };
    docker.check_available().await?;
    let stack = docker.config().await?;
    docker.project = stack.name;
    let service = stack
        .services
        .get(&spec.compose)
        .ok_or_else(|| ComposeError::UnknownService {
            service: spec.name.clone(),
            compose: spec.compose.clone(),
            dir: spec.project_dir.clone(),
        })?;
    for published in &spec.published {
        if !service.publishes(published.port) {
            return Err(ComposeError::NotPublished {
                service: spec.name.clone(),
                compose: spec.compose.clone(),
                port_name: published.port_name.clone(),
                port: published.port,
                upper: published.port_name.to_ascii_uppercase(),
                variables: spec.variables.join(", "),
            });
        }
    }
    // `--wait` returns once the container runs, or is healthy when the compose file defines a health check.
    let up = ["up", "--detach", "--wait", &spec.compose];
    let waited_out = match tokio::time::timeout(spec.health_timeout, docker.project_command(&up)).await {
        Ok(Ok(())) => None,
        Ok(Err(error)) => {
            // A container that started but never ran well would be left running with nobody counting it.
            let _ = docker.stop().await;
            return Err(error);
        }
        Err(_) => Some(health_timeout(spec.health_timeout)),
    };
    Ok((docker, waited_out))
}

fn health_timeout(timeout: Duration) -> ServiceEnd {
    ServiceEnd::HealthTimeout {
        timeout_ms: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
    }
}

/// Probes the health check of the Service until it passes, or `timeout` has passed.
async fn wait_healthy(health: &HealthCheck, timeout: Duration) -> Result<(), ServiceEnd> {
    let probing = async {
        while !health.passes().await {
            tokio::time::sleep(HEALTH_INTERVAL).await;
        }
    };
    tokio::time::timeout(timeout, probing)
        .await
        .map_err(|_| health_timeout(timeout))
}

impl Docker {
    /// Fails with what to install when `docker compose` v2 cannot run.
    async fn check_available(&self) -> Result<(), ComposeError> {
        let unavailable = |reason: String| ComposeError::Unavailable {
            service: self.service.clone(),
            reason,
        };
        let output = match self.output(&["version", "--short"]).await {
            Ok(output) => output,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(unavailable("docker is not on the PATH".to_owned()));
            }
            Err(error) => return Err(unavailable(format!("cannot run docker: {error}"))),
        };
        if !output.status.success() {
            return Err(unavailable(text(&output.stderr)));
        }
        let version = text(&output.stdout);
        match major(&version) {
            Some(major) if major < 2 => Err(unavailable(format!("found docker compose {version}"))),
            _ => Ok(()),
        }
    }

    /// The effective stack of the compose file of the Main worktree.
    async fn config(&self) -> Result<Stack, ComposeError> {
        let output = self.checked(&["config", "--format", "json"]).await?;
        serde_json::from_slice(&output.stdout).map_err(|source| ComposeError::Config {
            service: self.service.clone(),
            source,
        })
    }

    /// Whether the container runs, `false` when docker cannot tell.
    async fn is_running(&self) -> bool {
        let services = ["ps", "--status", "running", "--services"];
        match self.project_output(&services).await {
            Ok(output) => {
                output.status.success() && text(&output.stdout).lines().any(|service| service == self.compose)
            }
            Err(_) => false,
        }
    }

    /// The recent logs of the container.
    async fn logs(&self) -> String {
        let logs = ["logs", "--no-color", "--tail", LOG_LINES, &self.compose];
        match self.project_output(&logs).await {
            Ok(output) => {
                let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
                text.push_str(&String::from_utf8_lossy(&output.stderr));
                text
            }
            Err(_) => String::new(),
        }
    }

    /// Stops the container, keeping it and its volumes.
    async fn stop(&self) -> Result<(), ComposeError> {
        self.project_command(&["stop", &self.compose]).await
    }

    async fn project_command(&self, args: &[&str]) -> Result<(), ComposeError> {
        let output = self
            .project_output(args)
            .await
            .map_err(|error| ComposeError::Unavailable {
                service: self.service.clone(),
                reason: format!("cannot run docker: {error}"),
            })?;
        self.succeeded(args, &output)
    }

    async fn checked(&self, args: &[&str]) -> Result<Output, ComposeError> {
        let output = self.output(args).await.map_err(|error| ComposeError::Unavailable {
            service: self.service.clone(),
            reason: format!("cannot run docker: {error}"),
        })?;
        self.succeeded(args, &output)?;
        Ok(output)
    }

    fn succeeded(&self, args: &[&str], output: &Output) -> Result<(), ComposeError> {
        if output.status.success() {
            return Ok(());
        }
        Err(ComposeError::Command {
            service: self.service.clone(),
            args: args.join(" "),
            stderr: text(&output.stderr),
        })
    }

    /// Runs a command on the project once its name is known.
    async fn project_output(&self, args: &[&str]) -> std::io::Result<Output> {
        let mut command = vec!["--project-name", self.project.as_str()];
        command.extend_from_slice(args);
        self.output(&command).await
    }

    /// Runs `docker compose` with the arguments in the Main worktree, in the environment the compose file
    /// interpolates. The program is looked up on the `PATH` of that environment, which is the caller's.
    async fn output(&self, args: &[&str]) -> std::io::Result<Output> {
        Command::new("docker")
            .arg("compose")
            .args(args)
            .current_dir(&self.project_dir)
            .env_clear()
            .envs(self.env.iter().map(|(name, value)| (name, value)))
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output()
            .await
    }
}

/// The part of `docker compose config --format json` Procly reads.
#[derive(Debug, Deserialize)]
struct Stack {
    name: String,
    #[serde(default)]
    services: BTreeMap<String, StackService>,
}

#[derive(Debug, Deserialize)]
struct StackService {
    #[serde(default)]
    ports: Vec<StackPort>,
}

#[derive(Debug, Deserialize)]
struct StackPort {
    /// A port, or a range such as `8000-8010`, as text or as a number depending on the compose version.
    published: Option<HostPort>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum HostPort {
    Number(u64),
    Text(String),
}

impl StackService {
    /// Whether one of the ports of the service is published on `port` of the host.
    fn publishes(&self, port: u16) -> bool {
        self.ports
            .iter()
            .filter_map(|mapping| mapping.published.as_ref())
            .any(|published| {
                let text = match published {
                    HostPort::Number(number) => number.to_string(),
                    HostPort::Text(text) => text.clone(),
                };
                match text.split_once('-') {
                    Some((first, last)) => matches!(
                        (first.parse::<u16>(), last.parse::<u16>()),
                        (Ok(first), Ok(last)) if (first..=last).contains(&port)
                    ),
                    None => text.parse() == Ok(port),
                }
            })
    }
}

/// The major version in `2.29.1` or `v2.29.1`.
fn major(version: &str) -> Option<u32> {
    version.trim_start_matches('v').split('.').next()?.parse().ok()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service(ports: &str) -> StackService {
        serde_json::from_str(&format!("{{\"ports\": {ports}}}")).unwrap()
    }

    #[test]
    fn a_service_publishes_the_port_a_mapping_publishes() {
        let service = service(r#"[{"target": 80, "published": "24000"}, {"target": 81, "published": 24001}]"#);
        assert!(service.publishes(24000));
        assert!(service.publishes(24001));
        assert!(!service.publishes(80));
    }

    #[test]
    fn a_service_publishes_every_port_of_a_published_range() {
        let service = service(r#"[{"target": 80, "published": "8000-8010"}]"#);
        assert!(service.publishes(8000));
        assert!(service.publishes(8010));
        assert!(!service.publishes(8011));
    }

    #[test]
    fn a_mapping_without_a_host_port_publishes_nothing() {
        assert!(!service(r#"[{"target": 80}]"#).publishes(80));
        assert!(!service("[]").publishes(80));
    }

    #[test]
    fn the_major_version_of_compose_is_read_with_or_without_a_v() {
        assert_eq!(major("2.29.1"), Some(2));
        assert_eq!(major("v5.6.0"), Some(5));
        assert_eq!(major("1.29.2"), Some(1));
        assert_eq!(major("unknown"), None);
    }
}
