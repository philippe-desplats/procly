//! The Daemon: the single per-user authority, reachable only through a `0600` Unix socket (ADR 0003).

mod agent;
mod clock;
mod compose;
mod cpu;
mod environment;
mod health;
mod hook;
mod idle;
mod job;
mod lease;
mod listeners;
mod managed;
mod off;
mod process_group;
mod reconcile;
mod record;
mod requester;
mod service;
mod status;
mod store;
mod worktree_init;

use std::collections::{BTreeSet, HashMap};
use std::fs::{self, File, Permissions, TryLockError};
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context as _;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::signal::unix::{SignalKind, signal};

use self::clock::Clock;
use self::compose::{ComposeSpec, Composes, PlannedResource, Published, ResourcePlan};
use self::health::{HealthCheck, HttpCheck};
use self::job::{Fingerprint, JobSpec, Jobs};
use self::lease::Leases;
use self::managed::ManagedBlocks;
use self::off::Offs;
use self::record::ServiceRecords;
use self::service::{Service, ServiceSpec, Services, Start};
use crate::config;
use crate::ports;
use crate::protocol::{self, Released, Request, Response, ServiceEnd, StartOutcome, UpService};
use crate::state_dir::StateDir;
use crate::topology;

/// Every Job, Service and Lease the Daemon owns.
#[derive(Debug)]
struct Registry {
    jobs: Arc<Jobs>,
    services: Services,
    composes: Composes,
    leases: Arc<Leases>,
    managed: ManagedBlocks,
    offs: Offs,
    /// Held while the Rules block and the Guard are written or switched, so that an install and `off` never cross.
    agent: Mutex<()>,
    /// Held while the generated files of a Worktree are written, so that 2 starts never write them at once.
    mirroring: Mutex<()>,
    /// One lock per Worktree root, held while `worktree init` prepares it, so that 2 inits never both run a command.
    initializing: Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>,
    /// Set once the Services recorded by the previous Daemon are reconciled. Left unset when their document cannot
    /// be read, so that the request after it is fixed or removed reconciles again.
    reconciled: tokio::sync::OnceCell<()>,
}

impl Registry {
    /// Waits until the Services recorded by the previous Daemon are reconciled, which the first request touching
    /// Services runs if the Daemon has not finished it yet. A request that starts, stops or reports Services waits
    /// for it, so that it never starts a Service still running from before the restart.
    async fn reconciled(&self) -> Result<(), String> {
        self.reconciled
            .get_or_try_init(|| async {
                reconcile::run(&self.services)
                    .await
                    .map_err(|error| format!("{:#}", anyhow::Error::from(error)))
            })
            .await
            .map(|&()| ())
    }
}

impl Registry {
    /// The Worktree root and the name of every running Service, a container included under the Main worktree whose
    /// Leases it holds. Their Leases are in use and never expire.
    fn running(&self) -> BTreeSet<(PathBuf, String)> {
        let mut running = self.services.running();
        running.extend(self.composes.running());
        running
    }
}

/// Runs the Daemon until it receives `SIGTERM` or `SIGINT`.
///
/// Returns at once when another Daemon already holds the lock on this state directory.
pub(crate) fn run(state: &StateDir) -> anyhow::Result<()> {
    // Leave the session of the command that started us, so closing its terminal never reaches the Daemon.
    // This fails only when we already lead a process group, as when started by hand from a shell.
    let _ = nix::unistd::setsid();

    state.create()?;
    let lock_path = state.lock();
    let lock = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("cannot open the daemon lock {}", lock_path.display()))?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Ok(()),
        Err(TryLockError::Error(source)) => {
            return Err(source).with_context(|| format!("cannot lock {}", lock_path.display()));
        }
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("cannot start the async runtime")?;
    let result = runtime.block_on(serve(state));
    // Output readers of Jobs whose escaped descendants still hold their terminal never return, do not wait for them.
    runtime.shutdown_background();
    drop(lock);
    result
}

async fn serve(state: &StateDir) -> anyhow::Result<()> {
    let socket = state.socket();
    // Holding the lock makes any socket left here stale, from a Daemon that crashed.
    remove_if_present(&socket)?;
    let listener = UnixListener::bind(&socket).with_context(|| format!("cannot listen on {}", socket.display()))?;
    fs::set_permissions(&socket, Permissions::from_mode(0o600))
        .with_context(|| format!("cannot restrict {} to the user", socket.display()))?;
    fs::write(state.pid(), format!("{}\n", std::process::id()))
        .with_context(|| format!("cannot write {}", state.pid().display()))?;
    eprintln!("procly daemon {} listening on {}", std::process::id(), socket.display());

    let clock = Clock::from_env();
    let leases = Arc::new(Leases::new(state.leases(), clock.clone()));
    let registry = Arc::new(Registry {
        jobs: Arc::default(),
        services: Services::new(clock.clone(), ServiceRecords::new(state.services())),
        composes: Composes::new(clock.clone(), Arc::clone(&leases)),
        leases,
        managed: ManagedBlocks::new(state.managed_blocks()),
        offs: Offs::new(state.off()),
        agent: Mutex::default(),
        mirroring: Mutex::default(),
        initializing: Mutex::default(),
        reconciled: tokio::sync::OnceCell::new(),
    });
    // After the socket is bound, so that the command that started this Daemon is not left waiting to connect while
    // orphans are stopped, and its request waits for the reconciliation instead.
    tokio::spawn({
        let registry = Arc::clone(&registry);
        async move {
            if let Err(message) = registry.reconciled().await {
                eprintln!("cannot reconcile the services of the previous daemon: {message}");
            }
        }
    });
    let mut terminate = signal(SignalKind::terminate()).context("cannot handle SIGTERM")?;
    let mut interrupt = signal(SignalKind::interrupt()).context("cannot handle SIGINT")?;
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    tokio::spawn(handle(stream, Arc::clone(&registry)));
                }
                Err(error) => {
                    eprintln!("cannot accept a connection: {error}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            },
            _ = terminate.recv() => break,
            _ = interrupt.recv() => break,
        }
    }

    eprintln!("procly daemon {} stopping", std::process::id());
    // Signalling a Job's group sleeps between attempts, which must not park a runtime worker.
    let jobs = Arc::clone(&registry);
    let _ = tokio::task::spawn_blocking(move || jobs.jobs.kill_all()).await;
    registry.services.stop_all().await;
    registry.composes.stop_all().await;
    remove_if_present(&socket)?;
    remove_if_present(&state.pid())?;
    Ok(())
}

async fn handle(stream: UnixStream, registry: Arc<Registry>) {
    // Read from the kernel, so the Daemon log names the real requester whatever the request claims.
    let peer = stream.peer_cred().ok().and_then(|credentials| credentials.pid());
    let (reader, mut writer) = stream.into_split();
    let mut line = String::new();
    let response = match BufReader::new(reader).read_line(&mut line).await {
        Ok(_) => match protocol::decode::<Request>(&line) {
            Ok(request) => dispatch(request, &registry, peer).await,
            Err(error) => Response::Error {
                message: error.to_string(),
            },
        },
        Err(error) => {
            eprintln!("cannot read a request: {error}");
            return;
        }
    };
    // The client may be gone, as when an agent interrupts `procly wait`, nobody is left to tell.
    let line = protocol::encode(response).unwrap_or_else(|error| {
        protocol::encode(Response::Error {
            message: format!("cannot send the answer, a path in it is not valid UTF-8: {error}"),
        })
        .expect("an error answer holds only a string, which always serializes")
    });
    let _ = writer.write_all(line.as_bytes()).await;
}

/// Answers one request. `peer` is the pid of the requesting command, when the kernel tells it.
async fn dispatch(request: Request, registry: &Arc<Registry>, peer: Option<i32>) -> Response {
    let jobs = &registry.jobs;
    if matches!(
        request,
        Request::Up { .. } | Request::Down { .. } | Request::Status { .. }
    ) && let Err(message) = registry.reconciled().await
    {
        return Response::Error { message };
    }
    match request {
        Request::Run {
            command,
            cwd,
            env,
            timeout_ms,
        } => {
            let registry = Arc::clone(registry);
            // Resolving the Topology runs git, and opening a terminal and forking are blocking system calls.
            let started = tokio::task::spawn_blocking(move || {
                start_job(&registry.jobs, command, &cwd, env, Duration::from_millis(timeout_ms))
            });
            match started.await {
                Ok(Ok((job, outcome))) => Response::Run { job, outcome },
                Ok(Err(error)) => Response::Error {
                    message: format!("{error:#}"),
                },
                Err(error) => Response::Error {
                    message: format!("the job could not be started: {error}"),
                },
            }
        }
        Request::Wait { job: id } => match jobs.get(&id) {
            Some(job) => {
                let end = job.ended().await;
                Response::Ended {
                    output: job.output(),
                    end,
                }
            }
            None => unknown_job(&id),
        },
        Request::Logs { job: id } => match jobs.get(&id) {
            Some(job) => Response::Logs { output: job.output() },
            None => unknown_job(&id),
        },
        Request::Up {
            service: name,
            cwd,
            env,
            gitignore,
        } => up(registry, name, cwd, env, gitignore).await,
        Request::Env { service, cwd } => {
            let registry = Arc::clone(registry);
            // Resolving the Topology runs git, and reading the configuration and the Leases are blocking.
            match tokio::task::spawn_blocking(move || service_environment(&registry, &service, &cwd)).await {
                Ok(Ok(variables)) => Response::Env { variables },
                Ok(Err(error)) => Response::Error {
                    message: format!("{error:#}"),
                },
                Err(error) => Response::Error {
                    message: format!("the environment could not be read: {error}"),
                },
            }
        }
        Request::Down { service, cwd } => match down(Arc::clone(registry), service, cwd, peer).await {
            Ok((stopped, released)) => Response::Down { stopped, released },
            Err(error) => Response::Error {
                message: format!("{error:#}"),
            },
        },
        Request::Status { cwd, all } => {
            let registry = Arc::clone(registry);
            match tokio::task::spawn_blocking(move || status::report(&registry.jobs, &registry.services, &cwd, all))
                .await
            {
                Ok(Ok(report)) => Response::Status(report),
                Ok(Err(error)) => Response::Error {
                    message: format!("{error:#}"),
                },
                Err(error) => Response::Error {
                    message: format!("the status could not be read: {error}"),
                },
            }
        }
        Request::WorktreeInit { path, env } => match worktree_init::run(Arc::clone(registry), path, env).await {
            Ok(response) => response,
            Err(error) => Response::Error {
                message: format!("{error:#}"),
            },
        },
        request @ (Request::AgentInstall { .. } | Request::Switch { .. } | Request::Guard { .. }) => {
            agent::answer(registry, request).await
        }
    }
}

fn start_job(
    jobs: &Jobs,
    command: String,
    cwd: &Path,
    env: Vec<(String, String)>,
    timeout: Duration,
) -> anyhow::Result<(String, StartOutcome)> {
    let cwd = topology::canonicalize(cwd)?;
    // A Job runs anywhere, even where git fails, it is then deduplicated by its directory and command alone.
    let topology = topology::resolve(&cwd).unwrap_or_else(|error| {
        eprintln!(
            "cannot resolve the topology of {}, running the job outside any repository: {error}",
            cwd.display()
        );
        None
    });
    let spec = JobSpec {
        fingerprint: Fingerprint { topology, cwd, command },
        env,
        timeout,
    };
    Ok(jobs.start(&spec)?)
}

/// What a start request found out about the Service it asked for.
enum Started {
    Process(Start),
    /// A Compose service, whose container is started by [`up_compose`].
    Compose {
        topology: topology::Topology,
        grace: Duration,
    },
}

fn start_service(
    registry: &Arc<Registry>,
    name: &str,
    cwd: &Path,
    env: Vec<(String, String)>,
    gitignore: bool,
) -> anyhow::Result<Started> {
    let topology = topology_of(cwd)?;
    let worktree = topology.worktree.path.clone();
    {
        // Also on a Reuse, so that `--gitignore` is honored whenever it is asked. A Service that is not declared fails
        // here, before any file is written or Lease granted.
        let config = read_config(&worktree)?;
        let declared = declared(&config, &worktree, name)?;
        environment::check(name, declared)?;
        environment::mirror(registry, &topology, &config, gitignore)?;
        if let config::ServiceKind::Compose { .. } = declared.kind(name)? {
            return Ok(Started::Compose {
                grace: config.grace_period(declared),
                topology,
            });
        }
    }
    let start = registry.services.start::<anyhow::Error>(&worktree, name, || {
        let config = read_config(&worktree)?;
        let declared = declared(&config, &worktree, name)?;
        let config::ServiceKind::Process { run } = declared.kind(name)? else {
            anyhow::bail!("service {name} changed to a Compose service while it was starting");
        };
        // Granted under the start mutex, so that 2 Worktrees starting at once never get the same port.
        let injected = environment::injected(registry, &topology, &config, name, declared, true)?;
        // After the environment of the caller, so that a `PORT` of its own never wins over the Lease.
        let mut env = env;
        env.extend(injected.variables);
        Ok(ServiceSpec {
            command: ports::render(run, &injected.leases),
            cwd: worktree.clone(),
            env,
            health: HealthCheck {
                port: injected.health_port,
                http: declared.health.http().map(|(path, status)| HttpCheck { path, status }),
            },
            lease_ports: injected.leases.values().copied().collect(),
            health_timeout: config.health_timeout(declared),
            name: name.to_owned(),
            worktree: worktree.clone(),
            topology: topology.clone(),
            restart: config.restart_policy(declared),
            idle: config.idle_timeout(declared),
        })
    })?;
    if let Start::Ready {
        service,
        outcome: StartOutcome::Started,
    } = &start
    {
        // Dated again once it ends, so that a Service which ran longer than the expiry keeps its Leases.
        let (registry, service, name) = (Arc::clone(registry), Arc::clone(service), name.to_owned());
        tokio::spawn(async move {
            service.ended().await;
            let touched = tokio::task::spawn_blocking(move || registry.leases.touch(&worktree, &name)).await;
            if let Ok(Err(error)) = touched {
                eprintln!("cannot date the Leases of a service that ended: {error:#}");
            }
        });
    }
    Ok(Started::Process(start))
}

/// Starts the container of the Compose service `name`, or finds it running for another Worktree, and waits until it
/// is healthy.
async fn up_compose(
    registry: &Arc<Registry>,
    name: &str,
    topology: topology::Topology,
    grace: Duration,
    env: Vec<(String, String)>,
) -> Result<Running, Box<Response>> {
    let key = (topology.git_common_dir.clone(), name.to_owned());
    let worktree = topology.worktree.path.clone();
    let prepare = {
        let (registry, name, topology, env) = (Arc::clone(registry), name.to_owned(), topology.clone(), env.clone());
        move || prepare_compose(&registry, &name, &topology, env)
    };
    let plan = {
        let (registry, name) = (Arc::clone(registry), name.to_owned());
        move || plan_resources(&registry, &name, &topology, env)
    };
    match registry.composes.up(key, worktree, grace, prepare, plan).await {
        Ok(brought) => Ok(Running::Compose {
            outcome: brought.outcome,
            port: brought.port,
            health: brought.health,
        }),
        Err(error) => Err(Box::new(Response::Error {
            message: format!("{error:#}"),
        })),
    }
}

/// What it takes to start the container of the Compose service `name`. Run only when the container does not run,
/// so that the Leases of the Main worktree are granted anew then and never under a container using them.
fn prepare_compose(
    registry: &Registry,
    name: &str,
    topology: &topology::Topology,
    env: Vec<(String, String)>,
) -> anyhow::Result<ComposeSpec> {
    let worktree = &topology.worktree.path;
    let config = read_config(worktree)?;
    let declared = declared(&config, worktree, name)?;
    let config::ServiceKind::Compose { service } = declared.kind(name)? else {
        anyhow::bail!("service {name} changed to a Process service while it was starting");
    };
    // Granted under the lock of the container, so that 2 Worktrees starting at once never get different ports.
    let injected = environment::injected(registry, topology, &config, name, declared, true)?;
    let published = injected
        .leases
        .iter()
        .map(|(port_name, &port)| Published {
            port_name: port_name.clone(),
            port,
        })
        .collect();
    // What the compose file may interpolate a Lease from: `PORT_<NAME>`, and the Derived variables of the Service.
    let variables = injected
        .leases
        .keys()
        .map(|port_name| format!("PORT_{}", port_name.to_ascii_uppercase()))
        .chain(declared.env.keys().cloned())
        .collect();
    let mut env = env;
    env.extend(injected.variables);
    Ok(ComposeSpec {
        name: name.to_owned(),
        compose: service.to_owned(),
        project_dir: topology.main()?.worktree.path,
        env,
        published,
        variables,
        health: HealthCheck {
            port: injected.health_port,
            http: declared.health.http().map(|(path, status)| HttpCheck { path, status }),
        },
        health_timeout: config.health_timeout(declared),
    })
}

/// The Logical resources the Worktree needs in the container of the Compose service `name`, `None` when the service
/// has no Driver. Worked out on every `up`, a Reuse included, since each Worktree has resources of its own.
fn plan_resources(
    registry: &Arc<Registry>,
    name: &str,
    topology: &topology::Topology,
    env: Vec<(String, String)>,
) -> anyhow::Result<Option<ResourcePlan>> {
    let worktree = &topology.worktree.path;
    let config = read_config(worktree)?;
    let declared = declared(&config, worktree, name)?;
    if declared.driver.is_none() {
        return Ok(None);
    }
    // The Leases are the ones the container runs on, granted already, so they stay as they are.
    let injected = environment::injected(registry, topology, &config, name, declared, false)?;
    let Some((driver, resources)) = environment::logical_resources(topology, name, declared, &injected.leases)? else {
        return Ok(None);
    };
    let wanted = resources
        .into_iter()
        .map(|(key, resource)| PlannedResource {
            seed: declared.resources[&key].seed.clone(),
            key,
            resource,
        })
        .collect();
    let mut env = env;
    env.extend(injected.variables);
    Ok(Some(ResourcePlan {
        driver,
        topology: topology.clone(),
        resources: wanted,
        env,
        jobs: Arc::clone(&registry.jobs),
        ready_timeout: config.health_timeout(declared),
    }))
}

/// The variables Procly injects into the Service `name` of the Worktree holding `cwd`.
fn service_environment(registry: &Registry, name: &str, cwd: &Path) -> anyhow::Result<Vec<(String, String)>> {
    let topology = topology_of(cwd)?;
    let config = read_config(&topology.worktree.path)?;
    let declared = declared(&config, &topology.worktree.path, name)?;
    let mut variables = environment::injected(registry, &topology, &config, name, declared, false)?.variables;
    variables.sort();
    Ok(variables)
}

/// Stops the Services, logging who asked, and returns their names once their trees are gone.
async fn down(
    registry: Arc<Registry>,
    service: Option<String>,
    cwd: PathBuf,
    peer: Option<i32>,
) -> anyhow::Result<(Vec<String>, Vec<Released>)> {
    // Resolving the Topology runs git, and naming the requester runs ps.
    let (topology, who) = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        Ok((topology_of(&cwd)?, requester::describe(peer, &cwd)))
    })
    .await
    .context("the request could not be read")??;
    let worktree = topology.worktree.path.clone();
    let stopping = registry.services.stop(&worktree, service.as_deref());
    let released = registry
        .composes
        .release(&topology.git_common_dir, &worktree, service.as_deref());
    if let (Some(name), true) = (&service, stopping.is_empty() && released.is_empty()) {
        // Nothing to stop is fine for a declared Service, and a typo for any other.
        let (name, worktree) = (name.clone(), worktree.clone());
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            declared(&read_config(&worktree)?, &worktree, &name).map(|_| ())
        })
        .await
        .context("the configuration could not be read")??;
    }
    for (name, _) in &stopping {
        eprintln!("down {name} of {} requested by {who}", worktree.display());
    }
    let mut stopped = Vec::new();
    for (name, service) in stopping {
        service.ended().await;
        stopped.push(name);
    }
    for released in &released {
        eprintln!("down {} of {} requested by {who}", released.service, worktree.display());
    }
    Ok((stopped, released))
}

/// The root of the Worktree holding `cwd`, where its Services are declared.
fn worktree_of(cwd: &Path) -> anyhow::Result<PathBuf> {
    Ok(topology_of(cwd)?.worktree.path)
}

/// The Repository and the Worktree holding `cwd`, which must be in a git Repository.
fn topology_of(cwd: &Path) -> anyhow::Result<topology::Topology> {
    let cwd = topology::canonicalize(cwd)?;
    match topology::resolve(&cwd)? {
        Some(topology) => Ok(topology),
        None => anyhow::bail!(
            "{} is not in a git repository, and Services are declared per Repository",
            cwd.display()
        ),
    }
}

fn read_config(worktree: &Path) -> anyhow::Result<config::Config> {
    Ok(config::load(config::user_settings().as_deref(), worktree)?)
}

fn declared<'a>(config: &'a config::Config, worktree: &Path, name: &str) -> anyhow::Result<&'a config::ServiceConfig> {
    config.services.get(name).with_context(|| {
        format!(
            "no service {name} is declared in {}",
            worktree.join(config::VERSIONED_FILE).display()
        )
    })
}

/// Brings up the Services `name` needs, each one healthy before the next starts, then `name`, and answers with
/// where to reach it. Stops at the first Service that does not become healthy.
async fn up(
    registry: &Arc<Registry>,
    name: String,
    cwd: PathBuf,
    env: Vec<(String, String)>,
    gitignore: bool,
) -> Response {
    let planned = {
        let (cwd, name) = (cwd.clone(), name.clone());
        // Resolving the Topology runs git, and reading the configuration is blocking.
        tokio::task::spawn_blocking(move || start_order(&cwd, &name)).await
    };
    let order = match planned {
        Ok(Ok(order)) => order,
        Ok(Err(error)) => {
            return Response::Error {
                message: format!("{error:#}"),
            };
        }
        Err(error) => {
            return Response::Error {
                message: format!("the service could not be started: {error}"),
            };
        }
    };
    let Some((target, needed)) = order.split_last() else {
        return Response::Error {
            message: format!("no service {name} to start"),
        };
    };
    let mut needs = Vec::new();
    for current in needed.iter().chain(std::iter::once(target)) {
        let (outcome, port, health) = match start(registry, current, &cwd, &env, gitignore).await {
            Ok(Running::Process { service, outcome }) => {
                let health = service.healthy().await.map_err(|end| (end, service.output()));
                (outcome, service.port(), health)
            }
            Ok(Running::Compose { outcome, port, health }) => (outcome, port, health),
            Err(response) => return *response,
        };
        if let Err((end, output)) = health {
            return Response::Unhealthy {
                service: current.clone(),
                outcome,
                end,
                output,
                needed_by: (current != target).then(|| target.clone()),
            };
        }
        let reached = UpService {
            service: current.clone(),
            outcome,
            port,
            url: port.map(|port| format!("http://localhost:{port}")),
        };
        if current == target {
            return Response::Up {
                service: reached.service,
                outcome: reached.outcome,
                port: reached.port,
                url: reached.url,
                needs,
            };
        }
        needs.push(reached);
    }
    unreachable!("the loop returns on its last Service, which is the target")
}

/// The Services to bring up for `name`, needs first.
fn start_order(cwd: &Path, name: &str) -> anyhow::Result<Vec<String>> {
    let worktree = worktree_of(cwd)?;
    let config = read_config(&worktree)?;
    declared(&config, &worktree, name)?;
    Ok(config.start_order(name)?)
}

/// A Service that was started or found running.
enum Running {
    /// A Process service, whose health is still to be waited for.
    Process {
        service: Arc<Service>,
        outcome: StartOutcome,
    },
    /// A Compose service, already waited for: `health` tells whether its container answered in time, and if not,
    /// how it ended and its recent logs.
    Compose {
        outcome: StartOutcome,
        port: Option<u16>,
        health: Result<(), (ServiceEnd, String)>,
    },
}

/// Starts the Service `name`, or finds it running. A Service being stopped still holds its port, so this waits
/// for its tree to be gone and asks again.
async fn start(
    registry: &Arc<Registry>,
    name: &str,
    cwd: &Path,
    env: &[(String, String)],
    gitignore: bool,
) -> Result<Running, Box<Response>> {
    loop {
        let (blocking, name, cwd, env) = (Arc::clone(registry), name.to_owned(), cwd.to_owned(), env.to_owned());
        // Resolving the Topology runs git, and reading the configuration and forking are blocking.
        let started = tokio::task::spawn_blocking({
            let (name, env) = (name.clone(), env.clone());
            move || start_service(&blocking, &name, &cwd, env, gitignore)
        })
        .await;
        match started {
            Ok(Ok(Started::Process(Start::Ready { service, outcome }))) => {
                return Ok(Running::Process { service, outcome });
            }
            Ok(Ok(Started::Process(Start::Stopping(service)))) => service.ended().await,
            Ok(Ok(Started::Compose { topology, grace })) => {
                return up_compose(registry, &name, topology, grace, env).await;
            }
            Ok(Err(error)) => {
                return Err(Box::new(Response::Error {
                    message: format!("{error:#}"),
                }));
            }
            Err(error) => {
                return Err(Box::new(Response::Error {
                    message: format!("the service could not be started: {error}"),
                }));
            }
        }
    }
}

fn unknown_job(id: &str) -> Response {
    Response::Error {
        message: format!("no job {id}"),
    }
}

fn remove_if_present(path: &PathBuf) -> anyhow::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => {
            Err(error).with_context(|| format!("cannot remove {}", path.display()))
        }
        _ => Ok(()),
    }
}
