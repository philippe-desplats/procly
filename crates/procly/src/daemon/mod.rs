//! The Daemon: the single per-user authority, reachable only through a `0600` Unix socket (ADR 0003).

mod clock;
mod cpu;
mod health;
mod idle;
mod job;
mod lease;
mod listeners;
mod process_group;
mod requester;
mod service;
mod status;
mod store;
mod worktree_init;

use std::collections::HashMap;
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
use self::health::{HealthCheck, HttpCheck};
use self::job::{Fingerprint, JobSpec, Jobs};
use self::lease::{LeaseRequest, Leases};
use self::service::{Service, ServiceSpec, Services, Start};
use crate::config;
use crate::ports;
use crate::protocol::{self, Request, Response, StartOutcome, UpService};
use crate::state_dir::StateDir;
use crate::topology;

/// Every Job, Service and Lease the Daemon owns.
#[derive(Debug)]
struct Registry {
    jobs: Jobs,
    services: Services,
    leases: Leases,
    /// One lock per Worktree root, held while `worktree init` prepares it, so that 2 inits never both run a command.
    initializing: Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>,
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

    let registry = Arc::new(Registry {
        jobs: Jobs::default(),
        services: Services::new(Clock::from_env()),
        leases: Leases::new(state.leases()),
        initializing: Mutex::default(),
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
        } => up(registry, name, cwd, env).await,
        Request::Down { service, cwd } => match down(Arc::clone(registry), service, cwd, peer).await {
            Ok(stopped) => Response::Down { stopped },
            Err(error) => Response::Error {
                message: format!("{error:#}"),
            },
        },
        Request::Status { cwd, all } => {
            let registry = Arc::clone(registry);
            match tokio::task::spawn_blocking(move || status::report(&registry.jobs, &cwd, all)).await {
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

fn start_service(registry: &Registry, name: &str, cwd: &Path, env: Vec<(String, String)>) -> anyhow::Result<Start> {
    let topology = topology_of(cwd)?;
    let worktree = topology.worktree.path.clone();
    registry.services.start(&worktree, name, || {
        let config = read_config(&worktree)?;
        let declared = declared(&config, &worktree, name)?;
        // Checked before any Lease is granted, so that a request bound to fail stores none.
        let health_port_name = declared.health_port_name(name)?;
        ports::check(name, &declared.run, declared.ports.keys())?;
        // Granted under the start mutex, so that 2 Worktrees starting at once never get the same port.
        let leases = registry.leases.grant(&LeaseRequest {
            topology: &topology,
            service: name,
            ports: &declared.ports,
            range: config.settings.port_range,
            declared: &config.declared_ports(),
        })?;
        let health_port = health_port_name.map(|port_name| {
            *leases
                .get(port_name)
                .expect("a Lease is granted for every Port name, the health one included")
        });
        // After the environment of the caller, so that a `PORT` of its own never wins over the Lease.
        let mut env = env;
        env.extend(ports::environment(&leases, health_port));
        Ok(ServiceSpec {
            command: ports::render(&declared.run, &leases),
            cwd: worktree.clone(),
            env,
            health: HealthCheck {
                port: health_port,
                http: declared.health.http().map(|(path, status)| HttpCheck { path, status }),
            },
            lease_ports: leases.values().copied().collect(),
            health_timeout: config.health_timeout(declared),
            name: name.to_owned(),
            worktree: worktree.clone(),
            restart: config.restart_policy(declared),
            idle: config.idle_timeout(declared),
        })
    })
}

/// Stops the Services, logging who asked, and returns their names once their trees are gone.
async fn down(
    registry: Arc<Registry>,
    service: Option<String>,
    cwd: PathBuf,
    peer: Option<i32>,
) -> anyhow::Result<Vec<String>> {
    // Resolving the Topology runs git, and naming the requester runs ps.
    let (worktree, who) = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        Ok((worktree_of(&cwd)?, requester::describe(peer, &cwd)))
    })
    .await
    .context("the request could not be read")??;
    let stopping = registry.services.stop(&worktree, service.as_deref());
    if let (Some(name), true) = (&service, stopping.is_empty()) {
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
    Ok(stopped)
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
async fn up(registry: &Arc<Registry>, name: String, cwd: PathBuf, env: Vec<(String, String)>) -> Response {
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
        let (service, outcome) = match start(registry, current, &cwd, &env).await {
            Ok(started) => started,
            Err(response) => return *response,
        };
        if let Err(end) = service.healthy().await {
            return Response::Unhealthy {
                service: current.clone(),
                outcome,
                end,
                output: service.output(),
                needed_by: (current != target).then(|| target.clone()),
            };
        }
        let reached = UpService {
            service: current.clone(),
            outcome,
            port: service.port(),
            url: service.port().map(|port| format!("http://localhost:{port}")),
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

/// Starts the Service `name`, or finds it running. A Service being stopped still holds its port, so this waits
/// for its tree to be gone and asks again.
async fn start(
    registry: &Arc<Registry>,
    name: &str,
    cwd: &Path,
    env: &[(String, String)],
) -> Result<(Arc<Service>, StartOutcome), Box<Response>> {
    loop {
        let (registry, name, cwd, env) = (Arc::clone(registry), name.to_owned(), cwd.to_owned(), env.to_owned());
        // Resolving the Topology runs git, and reading the configuration and forking are blocking.
        let started = tokio::task::spawn_blocking(move || start_service(&registry, &name, &cwd, env)).await;
        match started {
            Ok(Ok(Start::Ready { service, outcome })) => return Ok((service, outcome)),
            Ok(Ok(Start::Stopping(service))) => service.ended().await,
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
