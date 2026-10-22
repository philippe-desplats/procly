//! Compose services in Shared mode: one container of the Repository's compose file serves every Worktree.
//!
//! Procly drives the compose file only through the `docker compose` v2 CLI and never rewrites it (ADR 0004). The
//! container is created from the compose file of the Main worktree under one project name, so that every Worktree
//! finds the same container. Worktrees are counted, and once the last one let go the container is stopped, never
//! removed, after its Grace period.
//!
//! A Compose service with a Driver is Logical: its container also holds a Logical resource per Worktree, which the
//! Driver creates through `docker compose exec` once the container is healthy, whichever Worktree started it.
//!
//! A Compose service that declares Isolated runs one container per Worktree instead, from the compose file of that
//! Worktree and under a compose project name of its own, on the Leases of that Worktree. Its compose file must leave
//! the names of the container and of its networks to compose. The container stops as soon as its Worktree no longer
//! needs it, there is nobody else to wait for.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde::Deserialize;
use tokio::process::Command;
use tokio::task::JoinSet;

use super::clock::Clock;
use super::container_record::{ContainerRecord, ContainerRecords, Key as ContainerKey};
use super::foreign::{self, Container as ListedContainer};
use super::health::HealthCheck;
use super::job::{Fingerprint, JobSpec, Jobs};
use super::lease::Leases;
use super::store::StoreError;
use crate::config::Seed;
use crate::derived::Resource;
use crate::driver::{self, Driver, DriverError};
use crate::duration;
use crate::protocol::{Conflict, ContainerStatus, Held, JobEnd, Released, ServiceEnd, StartOutcome};
use crate::topology::Topology;

/// Where to install `docker compose` v2, named in the error of a machine that lacks it.
const INSTALL_URL: &str = "https://docs.docker.com/compose/install/";
/// How long the health check waits between 2 probes.
const HEALTH_INTERVAL: Duration = Duration::from_millis(50);
/// How many lines of the logs of a container that never became healthy `up` shows.
const LOG_LINES: &str = "20";
/// How many lines of the logs of a container `procly logs` shows, its recent output as for a Process service.
const RECENT_LOG_LINES: &str = "1000";
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
    #[error(
        "service {service} is set to `isolation = \"isolated\"`, which runs one container per Worktree, but its \
         compose service {compose} fixes {what}, which 2 Worktrees cannot both use: {line}. Remove that line so that \
         compose names it after the project, or make the service Shared"
    )]
    FixedName {
        service: String,
        compose: String,
        what: String,
        line: String,
    },
    #[error("service {service} needs what runs outside Procly, which Procly never stops")]
    Conflict { service: String, conflicts: Vec<Conflict> },
    #[error(
        "the container {container} of service {service} was started outside Procly with the labels of its compose \
         service, but did not answer the health check of Procly within {timeout}, so it was not adopted and was left \
         running"
    )]
    AdoptedUnhealthy {
        service: String,
        container: String,
        timeout: String,
    },
    #[error("the service could not be started")]
    Prepare(#[source] tokio::task::JoinError),
    #[error("the resources could not be worked out")]
    Plan(#[source] tokio::task::JoinError),
}

/// Why `gc` could not drop a Logical resource.
#[derive(Debug, thiserror::Error)]
pub(crate) enum DropError {
    #[error(
        "the container of service {service} does not run, start it with `procly up {service}` in the Main worktree \
         and run `procly gc --apply` again"
    )]
    NotRunning { service: String },
    #[error(transparent)]
    Driver(#[from] DriverError),
    #[error("the {driver} Driver could not {step} it: {reason}, drop it by hand with `{by_hand}`")]
    Failed {
        driver: String,
        step: &'static str,
        reason: String,
        by_hand: String,
    },
    #[error("the drop could not be run: {0}")]
    Blocking(String),
}

/// The Logical resource `gc` drops: `name`, of the Compose service `service` the Worktrees of the Repository
/// `repository` share, on the Driver `driver`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ToDrop<'a> {
    pub(crate) repository: &'a Path,
    pub(crate) service: &'a str,
    pub(crate) name: &'a str,
    pub(crate) driver: &'a str,
}

/// What [`Composes::drop_resource`] did with a Logical resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Dropped {
    Dropped,
    /// Not dropped, it is no longer an Orphan.
    Held,
}

/// Runs `work` on a blocking thread.
async fn in_blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Result<T, DropError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| DropError::Blocking(error.to_string()))
}

/// What it takes to start the container of one Compose service.
#[derive(Debug)]
pub(crate) struct ComposeSpec {
    /// The name of the Service in `procly.toml`, named in errors.
    pub(crate) name: String,
    /// The service of the compose file the Service maps to.
    pub(crate) compose: String,
    /// Where the compose file is read from: the Main worktree, or the Worktree of an Isolated container.
    pub(crate) project_dir: PathBuf,
    /// `Some` when the container is Isolated, so that it gets a project name of its own.
    pub(crate) isolated: Option<Isolated>,
    /// The environment of the caller, then the Leases and the variables the compose file interpolates.
    pub(crate) env: Vec<(String, String)>,
    /// The Leases of the Service, which the compose file must publish.
    pub(crate) published: Vec<PublishedLease>,
    /// The names of the variables the compose file may interpolate the Leases from.
    pub(crate) variables: Vec<String>,
    /// The names of the variables Procly injected into `env`, which the record of the container keeps.
    pub(crate) injected: Vec<String>,
    pub(crate) health: HealthCheck,
    pub(crate) health_timeout: Duration,
}

/// Where the project name of an Isolated container comes from.
#[derive(Debug, Clone)]
pub(crate) struct Isolated {
    /// The Main worktree, whose compose file gives the project name of the Repository.
    pub(crate) main: PathBuf,
    /// The Worktree slug the project name of the Repository is followed by, `None` in the Main worktree, which keeps
    /// the project name of its compose file as it keeps its declared ports.
    pub(crate) slug: Option<String>,
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
    /// How long one command of the Driver may run in the container.
    pub(crate) command_timeout: Duration,
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
pub(crate) struct PublishedLease {
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

/// A Compose service is shared by the Worktrees of one Repository, so it is keyed by the Repository and its name, and
/// by the Worktree too when it is Isolated.
pub(crate) type ComposeKey = ContainerKey;

/// Every container Procly started from a compose file.
#[derive(Debug)]
pub(crate) struct Composes {
    ledger: Ledger,
    table: Mutex<HashMap<ComposeKey, Arc<Container>>>,
}

/// What a container is accounted in besides the table: the clock its Grace period runs on, the Leases dated when it
/// stops, and the record the next Daemon reconciles.
#[derive(Debug, Clone)]
struct Ledger {
    clock: Clock,
    /// Dated again when a container stops, so that its Leases do not expire the moment it does.
    leases: Arc<Leases>,
    records: Arc<ContainerRecords>,
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
    /// When the running Grace period ends, as time since the Unix epoch on the clock, `None` while a Worktree needs
    /// the container.
    deadline: Option<Duration>,
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
    /// The part of `env` the record keeps: what reaches the engine, and the variables Procly injected.
    recorded_env: Vec<(String, String)>,
}

impl Composes {
    pub(crate) fn new(clock: Clock, leases: Arc<Leases>, records: ContainerRecords) -> Self {
        Self {
            ledger: Ledger {
                clock,
                leases,
                records: Arc::new(records),
            },
            table: Mutex::default(),
        }
    }

    /// Reconciles the containers recorded by the previous Daemon with the ones Docker runs, before any request
    /// counts them. One that still runs with the labels of its compose service is re-attached with the Worktrees
    /// that need it, and its Grace period resumes from its recorded deadline, stopping it at once when the deadline
    /// passed while no Daemon ran. Any other is forgotten and never stopped, it is no longer the container Procly
    /// started.
    pub(crate) async fn reconcile(&self) -> Result<(), StoreError> {
        let records = Arc::clone(&self.ledger.records);
        // Docker is asked about each one, and whether its Worktrees still exist is read from the disk.
        let kept = tokio::task::spawn_blocking(move || -> Result<_, StoreError> {
            Ok(records.load()?.into_iter().filter_map(judge).collect::<Vec<_>>())
        })
        .await
        .expect("loading the container records never panics")?;
        let now = self.ledger.clock.since_epoch();
        let mut expiring = Vec::new();
        let mut recorded = Vec::new();
        for record in kept {
            let key = record.key();
            let container = Arc::new(Container::reattached(record, now));
            recorded.extend(container.record(&key));
            if let Some(deadline) = container.state().deadline {
                expiring.push((key.clone(), Arc::clone(&container), deadline));
            }
            self.table().insert(key, container);
        }
        let records = Arc::clone(&self.ledger.records);
        let _ = tokio::task::spawn_blocking(move || records.replace(recorded)).await;
        for (key, container, deadline) in expiring {
            let stopping = stop_after(self.ledger.clone(), key, container, deadline, 0);
            if deadline <= now {
                stopping.await;
            } else {
                tokio::spawn(stopping);
            }
        }
        Ok(())
    }

    /// Makes sure the container of the Compose service `key` runs for `worktree`, and waits until it is healthy.
    ///
    /// A container that Procly started and that still runs is a Reuse, whatever the Worktree. Otherwise `prepare`
    /// builds what it takes to start it, on a blocking thread. Either way, `plan` builds the Logical resources
    /// of the Worktree, which are created in the container when it lacks them. A failure of `prepare` or `plan`
    /// is returned as it is, so that the caller keeps its own error type.
    pub(crate) async fn up<E, P, R>(
        &self,
        key: ComposeKey,
        worktree: PathBuf,
        grace: Duration,
        prepare: P,
        plan: R,
    ) -> Result<Brought, E>
    where
        E: From<ComposeError> + Send + 'static,
        P: FnOnce() -> Result<ComposeSpec, E> + Send + 'static,
        R: FnOnce() -> Result<Option<ResourcePlan>, E> + Send + 'static,
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
            container.needed_by(alive, worktree);
            self.ledger.recorded(&key, &container).await;
            return Ok(Brought {
                outcome: StartOutcome::Reused,
                port,
                health: Ok(()),
            });
        }

        let spec = tokio::task::spawn_blocking(prepare)
            .await
            .map_err(ComposeError::Prepare)??;
        let port = spec.health.port;
        let began = Instant::now();
        let (docker, found) = start(&spec).await?;
        let waited_out = match found {
            Begun::Started(waited_out) => waited_out,
            Begun::Adopted(adopted) => {
                // Not adopted until it answers, so that a container Procly does not own is never stopped.
                if wait_healthy(&spec.health, spec.health_timeout).await.is_err() {
                    return Err(ComposeError::AdoptedUnhealthy {
                        service: spec.name.clone(),
                        container: adopted.name,
                        timeout: duration::format(spec.health_timeout),
                    }
                    .into());
                }
                docker.create_resources(planned(plan).await?.as_ref()).await?;
                eprintln!(
                    "adopted the container {} of service {}, started outside Procly with the labels of its compose \
                     service",
                    adopted.name, spec.name
                );
                container.needed_by(Alive { docker, port, grace }, worktree);
                self.ledger.recorded(&key, &container).await;
                return Ok(Brought {
                    outcome: StartOutcome::Reused,
                    port,
                    health: Ok(()),
                });
            }
        };
        let health = match waited_out {
            Some(end) => Err(end),
            // The health timeout covers `docker compose up --wait` and the probes together.
            None => wait_healthy(&spec.health, spec.health_timeout.saturating_sub(began.elapsed())).await,
        };
        if health.is_ok() {
            let created = match planned(plan).await {
                Ok(planned) => docker.create_resources(planned.as_ref()).await.map_err(E::from),
                Err(error) => Err(error),
            };
            if let Err(error) = created {
                let needed = !container.state().worktrees.is_empty();
                if needed {
                    // Restarted after it was stopped from outside Procly: the Worktrees that already need it keep it,
                    // whatever one Worktree could not create in it.
                    container.state().alive = Some(Alive { docker, port, grace });
                    self.ledger.recorded(&key, &container).await;
                } else {
                    // A container that started but cannot hold the resources would be left running with nobody
                    // counting it.
                    let _ = docker.stop().await;
                }
                return Err(error);
            }
        }
        if let Err(end) = health {
            let logs = docker.logs(LOG_LINES).await;
            // Stopped, never removed: the data stays, and the next `up` starts it again.
            let _ = docker.stop().await;
            return Ok(Brought {
                outcome: StartOutcome::Started,
                port,
                health: Err((end, logs)),
            });
        }
        container.needed_by(Alive { docker, port, grace }, worktree);
        self.ledger.recorded(&key, &container).await;
        Ok(Brought {
            outcome: StartOutcome::Started,
            port,
            health: Ok(()),
        })
    }

    /// The recent output of the container of the Compose service `name`, the service `compose` of the compose file
    /// of `project_dir`, the Main worktree or the Worktree of an `isolated` container, whether it runs or not. Read
    /// with `docker compose logs` in `env`, so that the compose file interpolates as it does for `up`.
    pub(crate) async fn logs(
        &self,
        name: &str,
        compose: &str,
        project_dir: PathBuf,
        isolated: Option<&Isolated>,
        env: Vec<(String, String)>,
    ) -> Result<String, ComposeError> {
        let mut docker = Docker {
            service: name.to_owned(),
            compose: compose.to_owned(),
            project_dir,
            project: String::new(),
            env,
            recorded_env: Vec::new(),
        };
        docker.check_available().await?;
        docker.stack(isolated).await?;
        Ok(docker.logs(RECENT_LOG_LINES).await)
    }

    /// Lets `worktree` go of the containers every Worktree of the Repository `repository` shares, of the Compose
    /// service `name` only when given. A Worktree whose directory no longer exists stops counting too. A container no
    /// Worktree needs anymore stops once its Grace period has passed on the clock. Blocking, it reads the disk and
    /// writes the record.
    pub(crate) fn release(&self, repository: &Path, worktree: &Path, name: Option<&str>) -> Vec<Released> {
        let mut released = Vec::new();
        let containers: Vec<_> = self
            .table()
            .iter()
            .filter(|((path, service, isolated), _)| {
                path == repository && isolated.is_none() && name.is_none_or(|name| name == service)
            })
            .map(|(key, container)| (key.clone(), Arc::clone(container)))
            .collect();
        for (key, container) in containers {
            let let_go = {
                let mut state = container.state();
                let Some(grace) = state.alive.as_ref().map(|alive| alive.grace) else {
                    continue;
                };
                let removed = forget_removed(&mut state.worktrees, &key.1);
                let let_go = state.worktrees.remove(worktree);
                if !let_go && !removed {
                    continue;
                }
                let others = state.worktrees.len();
                if others == 0 {
                    let deadline = self.ledger.clock.since_epoch().saturating_add(grace);
                    state.generation += 1;
                    state.deadline = Some(deadline);
                    tokio::spawn(stop_after(
                        self.ledger.clone(),
                        key.clone(),
                        Arc::clone(&container),
                        deadline,
                        state.generation,
                    ));
                }
                let_go.then(|| Released {
                    service: key.1.clone(),
                    others,
                    grace_ms: millis(grace),
                })
            };
            self.ledger.record(&key, &container);
            released.extend(let_go);
        }
        released.sort_by(|a, b| a.service.cmp(&b.service));
        released
    }

    /// The Isolated containers of the Repository `repository` that `worktree` lets go of: its own, of the Compose
    /// service `name` only when given, and those of every Worktree whose directory no longer exists, removed without
    /// `procly down`. Blocking, it reads the disk.
    pub(crate) fn isolated_released(&self, repository: &Path, worktree: &Path, name: Option<&str>) -> Vec<ComposeKey> {
        let keys: Vec<_> = self
            .table()
            .keys()
            .filter(|(path, _, isolated)| path == repository && isolated.is_some())
            .cloned()
            .collect();
        keys.into_iter()
            .filter(|(_, service, isolated)| {
                let Some(isolated) = isolated else { return false };
                (isolated == worktree && name.is_none_or(|name| name == service)) || !isolated.exists()
            })
            .collect()
    }

    /// Stops the Isolated containers `keys` at once, and returns the keys of those that were running.
    pub(crate) async fn stop_isolated(&self, keys: Vec<ComposeKey>) -> Result<Vec<ComposeKey>, ComposeError> {
        let containers: Vec<_> = {
            let table = self.table();
            keys.into_iter()
                .filter_map(|key| Some((key.clone(), Arc::clone(table.get(&key)?))))
                .collect()
        };
        let mut stopped = Vec::new();
        let mut failure = None;
        for (key, result) in self.stop_containers(containers).await {
            match result {
                Ok(true) => {
                    eprintln!(
                        "stopped the Isolated container of service {} of {}, its Worktree no longer needs it",
                        key.1,
                        key.2.as_deref().unwrap_or(Path::new("")).display()
                    );
                    stopped.push(key);
                }
                Ok(false) => {}
                Err(error) => failure = failure.or(Some(error)),
            }
        }
        stopped.sort();
        failure.map_or(Ok(stopped), Err)
    }

    /// The Isolated containers Procly started or adopted and that run, by the root of their Worktree.
    pub(crate) fn isolated(&self) -> Vec<(PathBuf, ContainerStatus)> {
        let mut isolated: Vec<_> = self
            .table()
            .iter()
            .filter_map(|((_, service, isolated), container)| {
                let worktree = isolated.clone()?;
                let state = container.state();
                let alive = state.alive.as_ref()?;
                Some((
                    worktree,
                    ContainerStatus {
                        service: service.clone(),
                        project: alive.docker.project.clone(),
                        compose: alive.docker.compose.clone(),
                        port: alive.port,
                    },
                ))
            })
            .collect();
        isolated.sort_by(|a, b| (&a.0, &a.1.service).cmp(&(&b.0, &b.1.service)));
        isolated
    }

    /// The Worktree root and the name of every container Procly started and that runs, which hold the Leases of the
    /// Main worktree, or of their own Worktree when they are Isolated.
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

    /// The compose project and service of every container Procly started or adopted and that runs.
    pub(crate) fn owned(&self) -> BTreeSet<(String, String)> {
        self.table()
            .values()
            .filter_map(|container| {
                let state = container.state();
                let alive = state.alive.as_ref()?;
                Some((alive.docker.project.clone(), alive.docker.compose.clone()))
            })
            .collect()
    }

    /// Drops the Logical resource `orphan.name` from the container the Worktrees of the Repository `orphan.repository`
    /// share for the Compose service `orphan.service`, through the `drop` command of its Driver, read from the Main
    /// worktree. Each command of the Driver may run for `timeout`.
    ///
    /// The container must run: it is never started for a drop. A resource the `check` of the Driver no longer finds
    /// counts as dropped, and once dropped `forget` forgets it, unless it tells, by returning `false`, that the resource
    /// is no longer an Orphan. The container is held throughout, so that no stop overlaps the drop, and so that no `up`
    /// creates or reuses the resource between `still_orphan`, which tells whether it is still an Orphan, and `forget`.
    /// Both are blocking and run on a blocking thread.
    pub(crate) async fn drop_resource<E>(
        &self,
        orphan: ToDrop<'_>,
        timeout: Duration,
        still_orphan: impl FnOnce() -> Result<bool, E> + Send + 'static,
        forget: impl FnOnce() -> Result<bool, E> + Send + 'static,
    ) -> Result<Dropped, E>
    where
        E: From<DropError> + Send + 'static,
    {
        let ToDrop {
            repository,
            service,
            name,
            driver,
        } = orphan;
        let not_running = || DropError::NotRunning {
            service: service.to_owned(),
        };
        let key = (repository.to_owned(), service.to_owned(), None);
        let container = self.table().get(&key).cloned().ok_or_else(not_running)?;
        let _commands = container.commands.lock().await;
        // Worked out again under the lock: a Worktree added since `gc` collected its Orphans may use the resource now.
        if !in_blocking(still_orphan).await?? {
            return Ok(Dropped::Held);
        }
        self.drop_held(name, driver, timeout, &container, not_running).await?;
        // A Worktree added again at the same path may hold the resource again: it is handed its index outside the lock.
        if !in_blocking(forget).await?? {
            return Ok(Dropped::Held);
        }
        Ok(Dropped::Dropped)
    }

    /// Drops the Logical resource `name` from `container`, whose lock the caller holds.
    async fn drop_held(
        &self,
        name: &str,
        driver: &str,
        timeout: Duration,
        container: &Container,
        not_running: impl Fn() -> DropError,
    ) -> Result<(), DropError> {
        let alive = container.state().alive.clone().ok_or_else(&not_running)?;
        let docker = alive.docker;
        if !docker.is_running().await {
            return Err(not_running());
        }
        let driver = {
            let (driver, main) = (driver.to_owned(), docker.project_dir.clone());
            // Reading the Driver file is blocking.
            tokio::task::spawn_blocking(move || Driver::load(&driver, &main, driver::user_dir().as_deref()))
                .await
                .expect("loading a Driver never panics")?
        };
        let script = |command: &str| Driver::command(command, name, None);
        let drop = script(&driver.drop);
        let failed = |step, reason| DropError::Failed {
            driver: driver.name.clone(),
            step,
            reason,
            by_hand: format!(
                "docker compose --project-name {} exec -T {} sh -c '{drop}'",
                docker.project, docker.compose
            ),
        };
        // A check that fails because the container cannot answer yet would take the resource for gone.
        if let Some(ready) = &driver.ready {
            docker
                .succeeds(timeout, ready)
                .await
                .map_err(|reason| failed("reach", reason))?;
        }
        match docker.exec_within(timeout, &script(&driver.check)).await {
            Ok(output) if !output.status.success() => return Ok(()),
            Ok(_) => {}
            Err(error) => return Err(failed("check", error.to_string())),
        }
        docker
            .succeeds(timeout, &drop)
            .await
            .map_err(|reason| failed("drop", reason))
    }

    /// Stops every container Procly started for the Repository `repository`, without waiting for any Grace period,
    /// and returns the names of the Compose services whose container it stopped, with the Worktree of an Isolated
    /// one. A container that was not running is not named. The containers are stopped, never removed, so their
    /// volumes stay.
    pub(crate) async fn stop_repository(
        &self,
        repository: &Path,
    ) -> Result<Vec<(String, Option<PathBuf>)>, ComposeError> {
        let containers: Vec<_> = self
            .table()
            .iter()
            .filter(|((path, _, _), _)| path == repository)
            .map(|(key, container)| (key.clone(), Arc::clone(container)))
            .collect();
        let mut stopped = Vec::new();
        let mut failure = None;
        for ((_, service, isolated), result) in self.stop_containers(containers).await {
            match result {
                Ok(true) => stopped.push((service, isolated)),
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
        containers: Vec<(ComposeKey, Arc<Container>)>,
    ) -> Vec<(ComposeKey, Result<bool, ComposeError>)> {
        let mut stopping = JoinSet::new();
        for (key, container) in containers {
            let ledger = self.ledger.clone();
            stopping.spawn(async move {
                let _commands = container.commands.lock().await;
                let alive = {
                    let mut state = container.state();
                    state.worktrees.clear();
                    state.generation += 1;
                    state.deadline = None;
                    state.alive.take()
                };
                let Some(alive) = alive else {
                    return (key, Ok(false));
                };
                let stopped = alive.docker.stop().await;
                if stopped.is_ok() {
                    touch(&ledger.leases, &alive).await;
                } else {
                    // Still counted as running, so that asking again stops it.
                    container.state().alive = Some(alive);
                }
                ledger.recorded(&key, &container).await;
                (key, stopped.map(|()| true))
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
    /// The container of a record the previous Daemon left, which runs. Its Grace period starts now when its last
    /// Worktrees were removed while no Daemon ran.
    fn reattached(record: ContainerRecord, now: Duration) -> Self {
        let grace = Duration::from_millis(record.grace_ms);
        let worktrees: BTreeSet<PathBuf> = record.worktrees.into_iter().collect();
        let deadline = match record.deadline_ms {
            Some(deadline) => Some(Duration::from_millis(deadline)),
            None if worktrees.is_empty() => Some(now.saturating_add(grace)),
            None => None,
        };
        let docker = Docker {
            service: record.service,
            compose: record.compose,
            project_dir: record.project_dir,
            project: record.project,
            env: record.env.clone(),
            recorded_env: record.env,
        };
        Self {
            commands: tokio::sync::Mutex::default(),
            state: Mutex::new(State {
                alive: Some(Alive {
                    docker,
                    port: record.port,
                    grace,
                }),
                worktrees,
                generation: 0,
                deadline,
            }),
        }
    }

    /// Counts `worktree` among the Worktrees that need the container, which runs as `alive`, ending any Grace period.
    fn needed_by(&self, alive: Alive, worktree: PathBuf) {
        let mut state = self.state();
        state.alive = Some(alive);
        state.worktrees.insert(worktree);
        state.generation += 1;
        state.deadline = None;
    }

    /// The record of the container of the Service `key`, `None` when it does not run.
    fn record(&self, key: &ComposeKey) -> Option<ContainerRecord> {
        let state = self.state();
        let alive = state.alive.as_ref()?;
        Some(ContainerRecord {
            repository: key.0.clone(),
            service: key.1.clone(),
            compose: alive.docker.compose.clone(),
            project: alive.docker.project.clone(),
            project_dir: alive.docker.project_dir.clone(),
            env: alive.docker.recorded_env.clone(),
            port: alive.port,
            grace_ms: millis(alive.grace),
            worktrees: state.worktrees.iter().cloned().collect(),
            deadline_ms: state.deadline.map(millis),
            isolated: key.2.clone(),
        })
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .expect("the container state mutex is never poisoned, no code panics while holding it")
    }
}

impl Ledger {
    /// Writes the record of the container of the Service `key` as it is now. Blocking.
    fn record(&self, key: &ComposeKey, container: &Container) {
        self.records.update(key.clone(), || container.record(key));
    }

    /// Writes the record of the container of the Service `key` as it is now, on a blocking thread.
    async fn recorded(&self, key: &ComposeKey, container: &Arc<Container>) {
        let (ledger, key, container) = (self.clone(), key.clone(), Arc::clone(container));
        let _ = tokio::task::spawn_blocking(move || ledger.record(&key, &container)).await;
    }
}

/// Compares the record of one container with what Docker runs, and keeps it, without the Worktrees whose directory
/// no longer exists, when it still runs with the labels of its compose service. Blocking.
fn judge(mut record: ContainerRecord) -> Option<ContainerRecord> {
    let label = format!(
        "the container of service {} (compose project {}, service {})",
        record.service, record.project, record.compose
    );
    let labels = [
        format!("label=com.docker.compose.project={}", record.project),
        format!("label=com.docker.compose.service={}", record.compose),
    ];
    match foreign::containers(&["--filter", &labels[0], "--filter", &labels[1]], Some(&record.env)) {
        Some(found)
            if found
                .iter()
                .any(|container| container.running && container.is_service(&record.project, &record.compose)) =>
        {
            eprintln!("{label} still runs after a restart of the Daemon, it is re-attached");
        }
        Some(_) => {
            eprintln!(
                "{label} no longer runs with the labels of its compose service after a restart of the Daemon, it is \
                 forgotten and left alone"
            );
            return None;
        }
        // Dropping the counts while the engine is down would stop the container too early once it is back.
        None => eprintln!("{label} could not be looked up, Docker did not answer, it is re-attached as recorded"),
    }
    let mut worktrees = record.worktrees.into_iter().collect();
    forget_removed(&mut worktrees, &record.service);
    record.worktrees = worktrees.into_iter().collect();
    Some(record)
}

/// Stops counting the Worktrees whose directory no longer exists, removed without `procly down`, and tells whether
/// there was one. Blocking.
fn forget_removed(worktrees: &mut BTreeSet<PathBuf>, service: &str) -> bool {
    let before = worktrees.len();
    worktrees.retain(|worktree| {
        let exists = worktree.exists();
        if !exists {
            eprintln!(
                "the Worktree {} no longer exists, it stops needing the container of service {service}",
                worktree.display()
            );
        }
        exists
    });
    worktrees.len() != before
}

/// What of the environment of `docker compose` the record of a container keeps: what reaches the engine, and the
/// variables Procly injected, named in `injected`. The rest is the caller's and may carry secrets.
fn recorded_env(env: &[(String, String)], injected: &[String]) -> Vec<(String, String)> {
    env.iter()
        .filter(|(name, _)| {
            matches!(name.as_str(), "PATH" | "HOME")
                || name.starts_with("DOCKER_")
                || name.starts_with("COMPOSE_")
                || injected.contains(name)
        })
        .cloned()
        .collect()
}

/// A duration in milliseconds, as the records and the answers carry it.
fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// What the Worktree needs in the container, built on a blocking thread.
async fn planned<E, R>(plan: R) -> Result<Option<ResourcePlan>, E>
where
    E: From<ComposeError> + Send + 'static,
    R: FnOnce() -> Result<Option<ResourcePlan>, E> + Send + 'static,
{
    tokio::task::spawn_blocking(plan).await.map_err(ComposeError::Plan)?
}

/// Stops the container of the Service `key` once the clock reaches `deadline`, unless a Worktree needed it again in
/// the meantime.
async fn stop_after(ledger: Ledger, key: ComposeKey, container: Arc<Container>, deadline: Duration, generation: u64) {
    let clock = &ledger.clock;
    clock.sleep(deadline.saturating_sub(clock.since_epoch())).await;
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
            {
                let mut state = container.state();
                state.alive = None;
                state.deadline = None;
            }
            ledger.recorded(&key, &container).await;
            touch(&ledger.leases, &alive).await;
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
async fn touch(leases: &Arc<Leases>, alive: &Alive) {
    let (leases, worktree, service) = (
        Arc::clone(leases),
        alive.docker.project_dir.clone(),
        alive.docker.service.clone(),
    );
    let touched = tokio::task::spawn_blocking(move || leases.touch(&worktree, &service)).await;
    if let Ok(Err(error)) = touched {
        eprintln!("cannot date the Leases of a container that stopped: {error:#}");
    }
}

/// How [`start`] found the container of a Compose service.
#[derive(Debug)]
enum Begun {
    /// Procly started it, with the end of the wait when it was still not running, or not healthy by its own health
    /// check, after the health timeout.
    Started(Option<ServiceEnd>),
    /// It already ran, started outside Procly, with the labels of the compose service and on its Leases.
    Adopted(ListedContainer),
}

/// Starts the container of the spec, or finds it running outside Procly for Adoption, and returns the commands that
/// drive it afterwards. Anything else holding its Leases or its container name is a conflict, and nothing is started.
async fn start(spec: &ComposeSpec) -> Result<(Docker, Begun), ComposeError> {
    let mut docker = Docker {
        service: spec.name.clone(),
        compose: spec.compose.clone(),
        project_dir: spec.project_dir.clone(),
        project: String::new(),
        env: spec.env.clone(),
        recorded_env: recorded_env(&spec.env, &spec.injected),
    };
    docker.check_available().await?;
    let stack = docker.stack(spec.isolated.as_ref()).await?;
    let service = stack
        .services
        .get(&spec.compose)
        .ok_or_else(|| ComposeError::UnknownService {
            service: spec.name.clone(),
            compose: spec.compose.clone(),
            dir: spec.project_dir.clone(),
        })?;
    if spec.isolated.is_some()
        && let Some((what, key, value)) = stack.fixed_name(service)
    {
        return Err(ComposeError::FixedName {
            service: spec.name.clone(),
            compose: spec.compose.clone(),
            what,
            line: offending_line(spec.project_dir.clone(), spec.env.clone(), key, value).await,
        });
    }
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
    let leased: Vec<u16> = spec.published.iter().map(|published| published.port).collect();
    let name = service
        .container_name
        .clone()
        .unwrap_or_else(|| format!("{}-{}-1", docker.project, spec.compose));
    match docker.occupants(&leased, name).await {
        Occupants::Service(found) => return Ok((docker, Begun::Adopted(found))),
        Occupants::Conflicts(conflicts) if !conflicts.is_empty() => {
            return Err(ComposeError::Conflict {
                service: spec.name.clone(),
                conflicts,
            });
        }
        Occupants::Conflicts(_) => {}
    }
    // `--wait` returns once the container runs, or is healthy when the compose file defines a health check. An
    // Isolated container starts alone: what it depends on would be started again in its project, on the same ports
    // as the containers of the Repository.
    let mut up = vec!["up", "--detach", "--wait"];
    if spec.isolated.is_some() {
        up.push("--no-deps");
    }
    up.push(&spec.compose);
    let waited_out = match tokio::time::timeout(spec.health_timeout, docker.project_command(&up)).await {
        Ok(Ok(())) => None,
        Ok(Err(error)) => {
            // A container that started but never ran well would be left running with nobody counting it.
            let _ = docker.stop().await;
            return Err(error);
        }
        Err(_) => Some(health_timeout(spec.health_timeout)),
    };
    Ok((docker, Begun::Started(waited_out)))
}

/// The compose files `docker compose` reads in a project directory when `COMPOSE_FILE` names none: the first of these,
/// and its override.
const COMPOSE_FILES: [(&str, &str); 4] = [
    ("compose.yaml", "compose.override.yaml"),
    ("compose.yml", "compose.override.yml"),
    ("docker-compose.yaml", "docker-compose.override.yaml"),
    ("docker-compose.yml", "docker-compose.override.yml"),
];

/// The line of the compose files of `project_dir` that sets `key` to `value`, as `<file>:<line>: <text>`, or the
/// setting alone when no line holds it as it is, such as one the compose file interpolates.
async fn offending_line(project_dir: PathBuf, env: Vec<(String, String)>, key: &'static str, value: String) -> String {
    let setting = format!("{key}: {value}");
    let found = tokio::task::spawn_blocking(move || {
        let files: Vec<PathBuf> = match env.iter().rev().find(|(name, _)| name == "COMPOSE_FILE") {
            Some((_, files)) => std::env::split_paths(files)
                .map(|file| project_dir.join(file))
                .collect(),
            None => COMPOSE_FILES
                .iter()
                .find(|(file, _)| project_dir.join(file).is_file())
                .map(|(file, over)| vec![project_dir.join(file), project_dir.join(over)])
                .unwrap_or_default(),
        };
        files.into_iter().find_map(|file| {
            let text = std::fs::read_to_string(&file).ok()?;
            text.lines().enumerate().find_map(|(index, line)| {
                let (found, rest) = line.trim().split_once(':')?;
                let rest = rest.trim().trim_matches(|c| c == '"' || c == '\'');
                (found == key && rest == value).then(|| format!("{}:{}: {}", file.display(), index + 1, line.trim()))
            })
        })
    })
    .await;
    found.ok().flatten().unwrap_or(setting)
}

/// What holds what the container of a Compose service needs.
#[derive(Debug)]
enum Occupants {
    /// The container of the service itself runs, started outside Procly, and publishes every Lease.
    Service(ListedContainer),
    /// What holds a Lease or the container name, empty when nothing does.
    Conflicts(Vec<Conflict>),
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
        log: None,
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
    let mut printed = trimmed_output(&output.stderr);
    if printed.is_empty() {
        printed = trimmed_output(&output.stdout);
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
            return Err(unavailable(trimmed_output(&output.stderr)));
        }
        let version = trimmed_output(&output.stdout);
        match major(&version) {
            Some(major) if major < 2 => Err(unavailable(format!("found docker compose {version}"))),
            _ => Ok(()),
        }
    }

    /// Names the compose project and returns its stack. The project is named by the compose file of the project
    /// directory, unless the container is `isolated` outside the Main worktree: it is then named by the compose file
    /// of the Main worktree, the project name of the Repository, followed by the Worktree slug.
    async fn stack(&mut self, isolated: Option<&Isolated>) -> Result<Stack, ComposeError> {
        if let Some(Isolated { main, slug: Some(slug) }) = isolated {
            let repository = Self {
                project_dir: main.clone(),
                ..self.clone()
            };
            self.project = format!("{}-{slug}", repository.config().await?.name);
        }
        let stack = self.config().await?;
        self.project.clone_from(&stack.name);
        Ok(stack)
    }

    /// The effective stack of the compose file of the project directory, under the project name when it is known.
    async fn config(&self) -> Result<Stack, ComposeError> {
        let args = ["config", "--format", "json"];
        let output = if self.project.is_empty() {
            self.checked(&args).await?
        } else {
            let output = self
                .project_output(&args)
                .await
                .map_err(|error| ComposeError::Unavailable {
                    service: self.service.clone(),
                    reason: format!("cannot run docker: {error}"),
                })?;
            self.succeeded(&args, &output)?;
            output
        };
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
                output.status.success()
                    && trimmed_output(&output.stdout)
                        .lines()
                        .any(|service| service == self.compose)
            }
            Err(_) => false,
        }
    }

    /// What holds the ports `leased` or the container name `name`, the container of the service included.
    ///
    /// The container of the service is the running one whose compose project and service labels are the expected ones
    /// exactly, `docker compose run` ones aside. On its Leases it is adopted, elsewhere it is a conflict: adopting it
    /// would move the Leases, and starting the service would recreate it.
    async fn occupants(&self, leased: &[u16], name: String) -> Occupants {
        let (project, compose, env, leased) = (
            self.project.clone(),
            self.compose.clone(),
            self.env.clone(),
            leased.to_owned(),
        );
        let found = tokio::task::spawn_blocking(move || {
            let labels = [
                format!("label=com.docker.compose.project={project}"),
                format!("label=com.docker.compose.service={compose}"),
            ];
            let of_service = foreign::containers(&["--filter", &labels[0], "--filter", &labels[1]], Some(&env))
                .unwrap_or_default()
                .into_iter()
                .find(|container| container.running && container.is_service(&project, &compose));
            if let Some(found) = of_service {
                if leased.iter().all(|port| found.ports.contains(port)) {
                    return Occupants::Service(found);
                }
                return Occupants::Conflicts(vec![Conflict {
                    held: Held::Service {
                        ports: found.ports.clone(),
                        leased,
                    },
                    occupant: found.occupant(),
                    stop: Some(format!("docker compose --project-name {project} stop {compose}")),
                }]);
            }
            let mut conflicts = Vec::new();
            let by_name = format!("name=^{name}$");
            let named = foreign::containers(&["--all", "--filter", &by_name], Some(&env)).unwrap_or_default();
            if let Some(holder) = named
                .into_iter()
                .find(|container| container.name == name && !container.is_service(&project, &compose))
            {
                conflicts.push(Conflict {
                    held: Held::ContainerName { name: name.clone() },
                    occupant: holder.occupant(),
                    stop: Some(format!("docker rm --force {name}")),
                });
            }
            for port in leased {
                if let Some((occupant, stop)) = foreign::occupant(port, Some(&env)) {
                    conflicts.push(Conflict {
                        held: Held::Port { port },
                        occupant,
                        stop,
                    });
                }
            }
            Occupants::Conflicts(conflicts)
        })
        .await;
        // A lookup that could not run finds nothing, and `docker compose up` reports what it runs into.
        found.unwrap_or_else(|_| Occupants::Conflicts(Vec::new()))
    }

    /// The recent logs of the container.
    async fn logs(&self, lines: &str) -> String {
        let logs = ["logs", "--no-color", "--tail", lines, &self.compose];
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
            match self.exec(plan, &script(&plan.driver.check)).await {
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
            let reason = match self.exec(plan, ready).await {
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
        let reason = match self.exec(plan, script).await {
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

    /// Runs a shell script of the Driver of `plan` in the container of the service, which must run.
    async fn exec(&self, plan: &ResourcePlan, script: &str) -> std::io::Result<Output> {
        self.exec_within(plan.command_timeout, script).await
    }

    /// Runs a shell script in the container of the service, failing with what it printed.
    async fn succeeds(&self, timeout: Duration, script: &str) -> Result<(), String> {
        match self.exec_within(timeout, script).await {
            Ok(output) if output.status.success() => Ok(()),
            Ok(output) => Err(failure(&output)),
            Err(error) => Err(error.to_string()),
        }
    }

    /// Runs a shell script in the container of the service, which must run, for `timeout` at most.
    async fn exec_within(&self, timeout: Duration, script: &str) -> std::io::Result<Output> {
        let args = ["exec", "-T", &self.compose, "sh", "-c", script];
        match tokio::time::timeout(timeout, self.project_output(&args)).await {
            Ok(output) => output,
            Err(_) => Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("it did not end within {}", duration::format(timeout)),
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
            stderr: trimmed_output(&output.stderr),
        })
    }

    /// Runs a command on the project once its name is known.
    async fn project_output(&self, args: &[&str]) -> std::io::Result<Output> {
        let mut command = vec!["--project-name", self.project.as_str()];
        command.extend_from_slice(args);
        self.output(&command).await
    }

    /// Runs `docker compose` with the arguments in the project directory, in the environment the compose file
    /// interpolates. The program is looked up on the `PATH` of that environment, which is the caller's.
    async fn output(&self, args: &[&str]) -> std::io::Result<Output> {
        // The Isolated container of a Worktree removed without `procly down` is still found by its project name.
        let project_dir = self.project_dir.clone();
        let dir = if tokio::task::spawn_blocking(move || project_dir.exists())
            .await
            .unwrap_or(false)
        {
            &self.project_dir
        } else {
            Path::new("/")
        };
        Command::new("docker")
            .arg("compose")
            .args(args)
            .current_dir(dir)
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
    #[serde(default)]
    networks: BTreeMap<String, StackNetwork>,
}

#[derive(Debug, Deserialize)]
struct StackService {
    #[serde(default)]
    ports: Vec<StackPort>,
    /// The name the compose file gives the container, which is `<project>-<service>-1` when it gives none.
    container_name: Option<String>,
    /// The networks the service joins by their key in the stack, `default` when it names none.
    networks: Option<BTreeMap<String, serde_json::Value>>,
}

#[derive(Debug, Deserialize)]
struct StackNetwork {
    /// The name of the network in the engine, which is `<project>_<key>` when the compose file gives none.
    name: Option<String>,
}

impl Stack {
    /// What the compose service `compose` has a fixed name for, which 2 projects cannot both use: its container, or
    /// a network it joins. Told as what it is, the key that fixes it and the name.
    fn fixed_name(&self, service: &StackService) -> Option<(String, &'static str, String)> {
        if let Some(name) = &service.container_name {
            return Some(("the name of its container".to_owned(), "container_name", name.clone()));
        }
        let default = BTreeMap::from([("default".to_owned(), serde_json::Value::Null)]);
        let joined = service.networks.as_ref().unwrap_or(&default);
        joined.keys().find_map(|key| {
            let name = self.networks.get(key)?.name.as_ref()?;
            (*name != format!("{}_{key}", self.name))
                .then(|| (format!("the name of its network {key}"), "name", name.clone()))
        })
    }
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

/// The output of a command as trimmed text, an invalid UTF-8 sequence read as a replacement character.
fn trimmed_output(bytes: &[u8]) -> String {
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

    fn fixed(stack: &str) -> Option<(String, &'static str, String)> {
        let stack: Stack = serde_json::from_str(stack).unwrap();
        stack.fixed_name(&stack.services["db"])
    }

    #[test]
    fn a_stack_whose_names_compose_gives_after_the_project_fixes_none() {
        assert_eq!(
            fixed(r#"{"name": "app", "services": {"db": {}}, "networks": {"default": {"name": "app_default"}}}"#),
            None
        );
        let back = r#"{"name": "app", "services": {"db": {"networks": {"back": null}}},
            "networks": {"default": {"name": "pinned"}, "back": {"name": "app_back"}}}"#;
        // A network the service does not join is none of its business.
        assert_eq!(fixed(back), None);
        // A service on the network of the host joins none of the stack.
        assert_eq!(
            fixed(r#"{"name": "app", "services": {"db": {"network_mode": "host"}}}"#),
            None
        );
    }

    #[test]
    fn a_container_name_or_the_name_of_a_joined_network_is_fixed() {
        assert_eq!(
            fixed(r#"{"name": "app", "services": {"db": {"container_name": "pg"}}}"#),
            Some((
                "the name of its container".to_owned(),
                "container_name",
                "pg".to_owned()
            ))
        );
        assert_eq!(
            fixed(r#"{"name": "app", "services": {"db": {}}, "networks": {"default": {"name": "shared"}}}"#),
            Some((
                "the name of its network default".to_owned(),
                "name",
                "shared".to_owned()
            ))
        );
    }

    #[tokio::test]
    async fn the_offending_line_is_found_in_the_compose_file_or_told_as_a_setting() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("compose.yaml"),
            "services:\n  db:\n    container_name: \"pg\"\n  cache:\n    container_name: ${NAME:-redis}\n",
        )
        .unwrap();
        let line = |value: &str| offending_line(dir.path().to_owned(), Vec::new(), "container_name", value.to_owned());

        assert_eq!(
            line("pg").await,
            format!(
                "{}:3: container_name: \"pg\"",
                dir.path().join("compose.yaml").display()
            )
        );
        // Interpolated, so no line of the file holds the name the stack gives.
        assert_eq!(line("redis").await, "container_name: redis");
    }

    #[test]
    fn a_mapping_without_a_host_port_publishes_nothing() {
        assert!(!service(r#"[{"target": 80}]"#).publishes(80));
        assert!(!service("[]").publishes(80));
    }

    #[test]
    fn the_record_keeps_what_reaches_the_engine_and_the_injected_variables_only() {
        let env: Vec<(String, String)> = [
            ("PATH", "/usr/bin"),
            ("HOME", "/home/dev"),
            ("DOCKER_HOST", "unix:///run/docker.sock"),
            ("COMPOSE_PROFILES", "dev"),
            ("API_TOKEN", "secret"),
            ("PORT_DB", "5432"),
            ("PORT_DB", "24000"),
            ("DATABASE_URL", "postgres://localhost:24000/app"),
        ]
        .into_iter()
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect();
        let injected = ["PORT_DB".to_owned(), "DATABASE_URL".to_owned()];

        let kept: Vec<_> = recorded_env(&env, &injected)
            .into_iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect();

        // The caller's own `PORT_DB` comes first and the injected one wins, as when `docker compose` ran.
        assert_eq!(
            kept,
            [
                "PATH=/usr/bin",
                "HOME=/home/dev",
                "DOCKER_HOST=unix:///run/docker.sock",
                "COMPOSE_PROFILES=dev",
                "PORT_DB=5432",
                "PORT_DB=24000",
                "DATABASE_URL=postgres://localhost:24000/app",
            ]
        );
    }

    #[test]
    fn the_major_version_of_compose_is_read_with_or_without_a_v() {
        assert_eq!(major("2.29.1"), Some(2));
        assert_eq!(major("v5.6.0"), Some(5));
        assert_eq!(major("1.29.2"), Some(1));
        assert_eq!(major("unknown"), None);
    }
}
