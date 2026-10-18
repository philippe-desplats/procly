//! Compose services in Shared mode: one container of the Repository's compose file serves every Worktree.
//!
//! Procly drives the compose file only through the `docker compose` v2 CLI and never rewrites it (ADR 0004). The
//! container is created from the compose file of the Main worktree under one project name, so that every Worktree
//! finds the same container. Worktrees are counted, and once the last one let go the container is stopped, never
//! removed, after its Grace period.
//!
//! A Compose service with a Driver is Logical: its container also holds a Logical resource per Worktree, which the
//! Driver creates through `docker compose exec` once the container is healthy, whichever Worktree started it.

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
use super::job::{Fingerprint, JobSpec, Jobs};
use super::lease::Leases;
use crate::config::Seed;
use crate::derived::Resource;
use crate::driver::Driver;
use crate::duration;
use crate::protocol::{JobEnd, Released, ServiceEnd, StartOutcome};
use crate::topology::Topology;

/// Where to install `docker compose` v2, named in the error of a machine that lacks it.
const INSTALL_URL: &str = "https://docs.docker.com/compose/install/";
/// How long the health check waits between 2 probes.
const HEALTH_INTERVAL: Duration = Duration::from_millis(50);
/// How many lines of the logs of a container that never became healthy `up` shows.
const LOG_LINES: &str = "20";
/// How long a command of a Driver may run in the container.
const DRIVER_COMMAND_TIMEOUT: Duration = Duration::from_mins(5);
/// How long the Driver waits between 2 probes of a container that is not ready to create resources.
const READY_INTERVAL: Duration = Duration::from_millis(250);
/// How many characters of the output of a failed command an error shows, its end being where the cause is.
const OUTPUT_TAIL: usize = 2000;

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
    #[error("the {driver} Driver of service {service} is not ready to create resources: {reason}")]
    NotReady {
        service: String,
        driver: String,
        reason: String,
    },
    #[error(
        "the {driver} Driver of service {service} has no `copy` command, so resource {name} cannot be a copy of {template}"
    )]
    NoCopy {
        service: String,
        driver: String,
        name: String,
        template: String,
    },
    #[error("the {driver} Driver of service {service} could not {step} resource {name}: {reason}")]
    Resource {
        service: String,
        driver: String,
        step: &'static str,
        name: String,
        reason: String,
    },
    #[error("the seed of resource {name} of service {service} failed: {reason}\n{outcome}")]
    SeedFailed {
        service: String,
        name: String,
        reason: String,
        outcome: String,
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

/// The Logical resources of one Worktree in the container of a Compose service, and what it takes to create them.
#[derive(Debug)]
pub(crate) struct ResourcePlan {
    pub(crate) driver: Driver,
    pub(crate) topology: Topology,
    pub(crate) resources: Vec<PlannedResource>,
    /// The environment of a command Seed: the caller's, then what the Service receives.
    pub(crate) env: Vec<(String, String)>,
    /// Where command Seeds run, as Jobs so that their process tree is killed on timeout or when the Daemon stops.
    pub(crate) jobs: Arc<Jobs>,
    /// How long the container may take to accept the commands of the Driver.
    pub(crate) ready_timeout: Duration,
}

/// A Logical resource a Worktree needs.
#[derive(Debug)]
pub(crate) struct PlannedResource {
    pub(crate) key: String,
    pub(crate) resource: Resource,
    pub(crate) seed: Seed,
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
    /// Dated again when a container stops, so that its Leases do not expire the moment it does.
    leases: Arc<Leases>,
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
    pub(crate) fn new(clock: Clock, leases: Arc<Leases>) -> Self {
        Self {
            clock,
            leases,
            table: Mutex::default(),
        }
    }

    /// Makes sure the container of the Compose service `key` runs for `worktree`, and waits until it is healthy.
    ///
    /// A container that Procly started and that still runs is a Reuse, whatever the Worktree. Otherwise `prepare`
    /// builds what it takes to start it, on a blocking thread. Either way, `plan` builds the Logical resources
    /// of the Worktree, which are created in the container when it lacks them.
    pub(crate) async fn up<P, R>(
        &self,
        key: ComposeKey,
        worktree: PathBuf,
        grace: Duration,
        prepare: P,
        plan: R,
    ) -> Result<Brought, ComposeError>
    where
        P: FnOnce() -> anyhow::Result<ComposeSpec> + Send + 'static,
        R: FnOnce() -> anyhow::Result<Option<ResourcePlan>> + Send + 'static,
    {
        let container = self.container(&key);
        let _commands = container.commands.lock().await;
        let alive = container.state().alive.clone();
        // A container stopped from outside Procly is started again below, for the Worktrees that already needed it too.
        if let Some(mut alive) = alive
            && alive.docker.is_running().await
        {
            // A Worktree whose resources cannot be created is not counted, it is not up.
            alive.docker.create_resources(planned(plan).await?.as_ref()).await?;
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
        if health.is_ok() {
            let created = match planned(plan).await {
                Ok(planned) => docker.create_resources(planned.as_ref()).await,
                Err(error) => Err(error),
            };
            if let Err(error) = created {
                let needed = !container.state().worktrees.is_empty();
                if needed {
                    // Restarted after it was stopped from outside Procly: the Worktrees that already need it keep it,
                    // whatever one Worktree could not create in it.
                    container.state().alive = Some(Alive { docker, port, grace });
                } else {
                    // A container that started but cannot hold the resources would be left running with nobody
                    // counting it.
                    let _ = docker.stop().await;
                }
                return Err(error);
            }
        }
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
                    Arc::clone(&self.leases),
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

    /// The Main worktree root and the name of every container Procly started and that runs, which hold the Leases of
    /// the Main worktree.
    pub(crate) fn running(&self) -> BTreeSet<(PathBuf, String)> {
        self.table()
            .values()
            .filter_map(|container| {
                let state = container.state();
                let alive = state.alive.as_ref()?;
                Some((alive.docker.project_dir.clone(), alive.docker.service.clone()))
            })
            .collect()
    }

    /// Stops every container Procly started, without waiting for any Grace period, for a Daemon about to exit.
    pub(crate) async fn stop_all(&self) {
        let containers: Vec<_> = self
            .table()
            .iter()
            .map(|(key, container)| (key.1.clone(), Arc::clone(container)))
            .collect();
        for (service, result) in self.stop_containers(containers).await {
            if let Err(error) = result {
                eprintln!("cannot stop the container of service {service}: {error}");
            }
        }
    }

    /// Stops every container Procly started for the Repository `repository`, without waiting for any Grace period,
    /// and returns the names of the Compose services whose container it stopped. A container that was not running
    /// is not named. The containers are stopped, never removed, so their volumes stay.
    pub(crate) async fn stop_repository(&self, repository: &Path) -> Result<Vec<String>, ComposeError> {
        let containers: Vec<_> = self
            .table()
            .iter()
            .filter(|((path, _), _)| path == repository)
            .map(|((_, service), container)| (service.clone(), Arc::clone(container)))
            .collect();
        let mut stopped = Vec::new();
        let mut failure = None;
        for (service, result) in self.stop_containers(containers).await {
            match result {
                Ok(true) => stopped.push(service),
                Ok(false) => {}
                Err(error) => failure = failure.or(Some(error)),
            }
        }
        stopped.sort();
        failure.map_or(Ok(stopped), Err)
    }

    /// Stops the containers at once, all together, and tells for each one whether it was running.
    async fn stop_containers(
        &self,
        containers: Vec<(String, Arc<Container>)>,
    ) -> Vec<(String, Result<bool, ComposeError>)> {
        let mut stopping = JoinSet::new();
        for (service, container) in containers {
            let leases = Arc::clone(&self.leases);
            stopping.spawn(async move {
                let _commands = container.commands.lock().await;
                let alive = {
                    let mut state = container.state();
                    state.worktrees.clear();
                    state.generation += 1;
                    state.alive.take()
                };
                let Some(alive) = alive else {
                    return (service, Ok(false));
                };
                match alive.docker.stop().await {
                    Ok(()) => {
                        touch(leases, &alive).await;
                        (service, Ok(true))
                    }
                    Err(error) => {
                        // Still counted as running, so that asking again stops it.
                        container.state().alive = Some(alive);
                        (service, Err(error))
                    }
                }
            });
        }
        let mut results = Vec::new();
        while let Some(result) = stopping.join_next().await {
            if let Ok(result) = result {
                results.push(result);
            }
        }
        results
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

/// What the Worktree needs in the container, built on a blocking thread.
async fn planned<R>(plan: R) -> Result<Option<ResourcePlan>, ComposeError>
where
    R: FnOnce() -> anyhow::Result<Option<ResourcePlan>> + Send + 'static,
{
    Ok(tokio::task::spawn_blocking(plan)
        .await
        .map_err(|error| anyhow::anyhow!("the resources could not be worked out: {error}"))??)
}

/// Stops the container after `grace`, unless a Worktree needed it again in the meantime.
async fn stop_after(clock: Clock, leases: Arc<Leases>, container: Arc<Container>, grace: Duration, generation: u64) {
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
            touch(leases, &alive).await;
            eprintln!(
                "stopped the container of service {}, no Worktree needs it",
                alive.docker.service
            );
        }
        // Still counted as running, so that the next `up` finds it and the next release schedules another stop.
        Err(error) => eprintln!("cannot stop the container of service {}: {error}", alive.docker.service),
    }
}

/// Dates the Leases of a container that just stopped as used now, so that they do not expire at once.
async fn touch(leases: Arc<Leases>, alive: &Alive) {
    let (worktree, service) = (alive.docker.project_dir.clone(), alive.docker.service.clone());
    let touched = tokio::task::spawn_blocking(move || leases.touch(&worktree, &service)).await;
    if let Ok(Err(error)) = touched {
        eprintln!("cannot date the Leases of a container that stopped: {error:#}");
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

/// Runs the command Seed of a resource on the host, at the root of the Worktree, as a Job. It reads the resource in
/// `PROCLY_RESOURCE_URL`, `PROCLY_RESOURCE_NAME` and `PROCLY_RESOURCE`, the key of the resource.
async fn run_seed(plan: &ResourcePlan, wanted: &PlannedResource, run: &str, timeout: Duration) -> Result<(), String> {
    let mut env = plan.env.clone();
    env.extend([
        ("PROCLY_RESOURCE".to_owned(), wanted.key.clone()),
        ("PROCLY_RESOURCE_NAME".to_owned(), wanted.resource.name.clone()),
        ("PROCLY_RESOURCE_URL".to_owned(), wanted.resource.url.clone()),
    ]);
    let spec = JobSpec {
        fingerprint: Fingerprint {
            topology: Some(plan.topology.clone()),
            cwd: plan.topology.worktree.path.clone(),
            command: run.to_owned(),
        },
        env,
        timeout,
    };
    let jobs = Arc::clone(&plan.jobs);
    // Opening a terminal and forking are blocking system calls.
    let (id, _) = tokio::task::spawn_blocking(move || jobs.start(&spec))
        .await
        .map_err(|error| format!("cannot run the seed command: {error}"))?
        .map_err(|error| format!("cannot run the seed command: {error}"))?;
    let job = plan
        .jobs
        .get(&id)
        .ok_or_else(|| "the seed command vanished from the job table".to_owned())?;
    let reason = match job.ended().await {
        JobEnd::Exited { code: 0 } => return Ok(()),
        JobEnd::Exited { code } => format!("{run:?} exited with {code}"),
        JobEnd::Signaled { signal } => format!("{run:?} was killed by signal {signal}"),
        JobEnd::TimedOut { .. } => format!("{run:?} did not end within {}", duration::format(timeout)),
    };
    let output = tail(&job.output());
    Err(if output.is_empty() {
        reason
    } else {
        format!("{reason}: {output}")
    })
}

/// How a command that failed ended, with the end of what it printed.
fn failure(output: &Output) -> String {
    let mut printed = text(&output.stderr);
    if printed.is_empty() {
        printed = text(&output.stdout);
    }
    format!("{}: {}", output.status, tail(&printed))
}

/// The last [`OUTPUT_TAIL`] characters of `text`.
fn tail(text: &str) -> String {
    let text = text.trim();
    let skip = text.chars().count().saturating_sub(OUTPUT_TAIL);
    text.chars().skip(skip).collect()
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

    /// Creates the Logical resources of the Worktree the container lacks, each one filled by its Seed.
    ///
    /// A resource that exists is left as it is, whatever it holds. One created here whose command Seed fails is
    /// dropped again, so that the next `up` seeds it afresh instead of finding it and keeping it empty.
    async fn create_resources(&self, plan: Option<&ResourcePlan>) -> Result<(), ComposeError> {
        let Some(plan) = plan else {
            return Ok(());
        };
        if let Some(ready) = &plan.driver.ready {
            self.wait_ready(plan, ready).await?;
        }
        for wanted in &plan.resources {
            let name = &wanted.resource.name;
            let template = wanted.seed.template();
            let script = |command: &str| Driver::command(command, name, template);
            // A check that cannot run says nothing about the resource: creating on its word could hit one that exists,
            // and a failed Seed would then drop it.
            match self.exec(&script(&plan.driver.check)).await {
                Ok(output) if output.status.success() => continue,
                Ok(_) => {}
                Err(error) => {
                    return Err(ComposeError::Resource {
                        service: self.service.clone(),
                        driver: plan.driver.name.clone(),
                        step: "check",
                        name: name.clone(),
                        reason: error.to_string(),
                    });
                }
            }
            let creating = match (template, &plan.driver.copy) {
                (Some(_), Some(copy)) => copy,
                (Some(template), None) => {
                    return Err(ComposeError::NoCopy {
                        service: self.service.clone(),
                        driver: plan.driver.name.clone(),
                        name: name.clone(),
                        template: template.to_owned(),
                    });
                }
                (None, _) => &plan.driver.create,
            };
            self.driver_step(plan, "create", name, &script(creating)).await?;
            if let Seed::Command { run, timeout } = &wanted.seed
                && let Err(reason) = run_seed(plan, wanted, run, *timeout).await
            {
                let outcome = self.drop_created(plan, name, &script(&plan.driver.drop)).await;
                eprintln!(
                    "the seed of resource {name} of service {} failed ({reason}), {outcome}",
                    self.service
                );
                return Err(ComposeError::SeedFailed {
                    service: self.service.clone(),
                    name: name.clone(),
                    reason,
                    outcome,
                });
            }
            eprintln!("created resource {name} of service {}", self.service);
        }
        Ok(())
    }

    /// Drops a resource this very request created, and says what became of it.
    async fn drop_created(&self, plan: &ResourcePlan, name: &str, script: &str) -> String {
        match self.driver_step(plan, "drop", name, script).await {
            Ok(()) => format!(
                "the resource {name} was created by this request and has been dropped, so the next `procly up` seeds it again"
            ),
            Err(error) => format!(
                "the resource {name} was created by this request and could not be dropped ({error:#}), drop it with `docker compose exec {} sh -c '{script}'` before the next `procly up`",
                self.compose
            ),
        }
    }

    /// Polls the container until the Driver finds it ready to create resources, or `ready_timeout` has passed.
    async fn wait_ready(&self, plan: &ResourcePlan, ready: &str) -> Result<(), ComposeError> {
        let deadline = Instant::now() + plan.ready_timeout;
        loop {
            let reason = match self.exec(ready).await {
                Ok(output) if output.status.success() => return Ok(()),
                Ok(output) => failure(&output),
                Err(error) => error.to_string(),
            };
            if Instant::now() >= deadline {
                return Err(ComposeError::NotReady {
                    service: self.service.clone(),
                    driver: plan.driver.name.clone(),
                    reason,
                });
            }
            tokio::time::sleep(READY_INTERVAL).await;
        }
    }

    /// Runs a command of the Driver in the container, failing with what it printed.
    async fn driver_step(
        &self,
        plan: &ResourcePlan,
        step: &'static str,
        name: &str,
        script: &str,
    ) -> Result<(), ComposeError> {
        let reason = match self.exec(script).await {
            Ok(output) if output.status.success() => return Ok(()),
            Ok(output) => failure(&output),
            Err(error) => error.to_string(),
        };
        Err(ComposeError::Resource {
            service: self.service.clone(),
            driver: plan.driver.name.clone(),
            step,
            name: name.to_owned(),
            reason,
        })
    }

    /// Runs a shell script in the container of the service, which must run.
    async fn exec(&self, script: &str) -> std::io::Result<Output> {
        let args = ["exec", "-T", &self.compose, "sh", "-c", script];
        match tokio::time::timeout(DRIVER_COMMAND_TIMEOUT, self.project_output(&args)).await {
            Ok(output) => output,
            Err(_) => Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("it did not end within {}", duration::format(DRIVER_COMMAND_TIMEOUT)),
            )),
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
