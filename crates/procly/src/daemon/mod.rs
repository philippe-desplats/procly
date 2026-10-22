//! The Daemon: the single per-user authority, reachable only through a `0600` Unix socket (ADR 0003).

mod agent;
mod atomic_file;
pub(crate) mod clock;
mod compose;
mod container_record;
mod cpu;
mod eject;
mod environment;
mod foreign;
mod gc;
mod health;
mod hook;
mod idle;
mod index;
mod isolated_record;
mod job;
mod lease;
mod listeners;
mod managed;
mod off;
mod output_file;
#[cfg(unix)]
mod process_group;
mod reconcile;
mod record;
mod requester;
mod resource;
mod retention;
mod service;
mod status;
mod store;
mod up;
mod worktree_dir;
mod worktree_init;
mod worktree_release;

use std::collections::{BTreeSet, HashMap};
use std::fs::{self, File, Permissions, TryLockError};
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context as _;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::{UnixListener, UnixStream};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc::{self, UnboundedSender};

use self::clock::Clock;
use self::compose::Composes;
use self::container_record::ContainerRecords;
use self::index::Indexes;
use self::isolated_record::IsolatedRecords;
use self::job::{Fingerprint, JobSpec, Jobs};
use self::lease::Leases;
use self::managed::ManagedBlocks;
use self::off::Offs;
use self::output_file::OutputFiles;
use self::record::ServiceRecords;
use self::resource::LogicalResources;
use self::retention::Retentions;
use self::service::Services;
use crate::config;
use crate::protocol::{self, Released, Request, Response, StartOutcome};
use crate::state_dir::StateDir;
use crate::topology;

/// Every Job, Service and Lease the Daemon owns.
#[derive(Debug)]
struct Registry {
    jobs: Arc<Jobs>,
    services: Services,
    composes: Composes,
    leases: Arc<Leases>,
    /// Every Isolated container, running or stopped, which `gc` removes once its Worktree is.
    isolated: Arc<IsolatedRecords>,
    /// The delayed deletions of Isolated containers whose Retention policy is a duration.
    retentions: Retentions,
    indexes: Indexes,
    /// The Logical resources of every Worktree other than the Main worktree, which `gc` drops once it is removed.
    resources: LogicalResources,
    managed: ManagedBlocks,
    offs: Offs,
    /// Held while the Rules block and the Guard are written or switched, so that an install and `off` never cross.
    agent: Mutex<()>,
    /// Held while the generated files of a Worktree are written, so that 2 starts never write them at once.
    mirroring: Mutex<()>,
    /// One lock per Worktree root, held while `worktree init` prepares it, so that 2 inits never both run a command.
    initializing: Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>,
    /// The directory holding the setup directory of each Worktree, with the log of the setup commands `worktree init`
    /// runs.
    setup_logs: PathBuf,
    /// Set once the Services recorded by the previous Daemon are reconciled. Left unset when their document cannot
    /// be read, so that the request after it is fixed or removed reconciles again.
    reconciled: tokio::sync::OnceCell<()>,
    /// Set once the containers recorded by the previous Daemon are reconciled, apart from the Services, so that a
    /// container document that cannot be read never has the Services re-attached twice.
    containers_reconciled: tokio::sync::OnceCell<()>,
}

impl Registry {
    /// Waits until the Services and the containers recorded by the previous Daemon are reconciled, which the first
    /// request touching Services runs if the Daemon has not finished it yet. A request that starts, stops or reports
    /// Services waits for it, so that it never starts a Service still running from before the restart, nor loses
    /// count of the Worktrees that need a container.
    async fn reconciled(&self) -> Result<(), String> {
        self.reconciled
            .get_or_try_init(|| async { reconcile::run(&self.services).await.map_err(message) })
            .await?;
        self.containers_reconciled
            .get_or_try_init(|| async { self.composes.reconcile().await.map_err(message) })
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

/// Appends the standard input to the output file at `path` until it ends, keeping the file bounded: the writer of the
/// output file of a Service, see [`output_file`].
pub(crate) fn write_output(path: &Path) -> anyhow::Result<()> {
    output_file::write(io::stdin().lock(), path)
        .with_context(|| format!("cannot write the output file {}", path.display()))
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
    let isolated = Arc::new(IsolatedRecords::new(state.isolated()));
    let registry = Arc::new(Registry {
        jobs: Arc::default(),
        services: Services::new(
            clock.clone(),
            ServiceRecords::new(state.services()),
            OutputFiles::new(state.outputs()),
        ),
        composes: Composes::new(
            clock.clone(),
            Arc::clone(&leases),
            ContainerRecords::new(state.containers()),
            Arc::clone(&isolated),
        ),
        isolated,
        retentions: Retentions::new(clock.clone()),
        leases,
        indexes: Indexes::new(state.indexes()),
        resources: LogicalResources::new(state.resources()),
        managed: ManagedBlocks::new(state.managed_blocks()),
        offs: Offs::new(state.off()),
        agent: Mutex::default(),
        mirroring: Mutex::default(),
        initializing: Mutex::default(),
        setup_logs: state.setup_logs(),
        reconciled: tokio::sync::OnceCell::new(),
        containers_reconciled: tokio::sync::OnceCell::new(),
    });
    // After the socket is bound, so that the command that started this Daemon is not left waiting to connect while
    // orphans are stopped, and its request waits for the reconciliation instead.
    tokio::spawn({
        let registry = Arc::clone(&registry);
        async move {
            if let Err(message) = registry.reconciled().await {
                eprintln!("cannot reconcile the services and containers of the previous daemon: {message}");
            }
            // Once the containers of removed Worktrees are stopped, which a policy never deletes while they run.
            retention::sweep(&registry).await;
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
    // The containers keep running, recorded with the Worktrees that need them, so that the next Daemon resumes their
    // counts and Grace periods.
    remove_if_present(&socket)?;
    remove_if_present(&state.pid())?;
    Ok(())
}

async fn handle(stream: UnixStream, registry: Arc<Registry>) {
    // Read from the kernel, so the Daemon log names the real requester whatever the request claims.
    let peer = stream.peer_cred().ok().and_then(|credentials| credentials.pid());
    let (reader, mut writer) = stream.into_split();
    let mut line = String::new();
    let (progress, mut progressing) = mpsc::unbounded_channel();
    let response = match BufReader::new(reader).read_line(&mut line).await {
        Ok(_) => match protocol::decode::<Request>(&line) {
            Ok(request) => {
                let dispatching = dispatch(request, &registry, peer, &progress);
                tokio::pin!(dispatching);
                // The progress of a long request goes out ahead of its answer, one line each.
                loop {
                    tokio::select! {
                        response = &mut dispatching => break response,
                        Some(update) = progressing.recv() => send(&mut writer, update).await,
                    }
                }
            }
            Err(error) => Response::Error {
                message: error.to_string(),
            },
        },
        Err(error) => {
            eprintln!("cannot read a request: {error}");
            return;
        }
    };
    // What a request sent just before it answered goes out ahead of the answer.
    while let Ok(update) = progressing.try_recv() {
        send(&mut writer, update).await;
    }
    send(&mut writer, response).await;
}

/// Writes one line to the client.
async fn send(writer: &mut OwnedWriteHalf, response: Response) {
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
async fn dispatch(
    request: Request,
    registry: &Arc<Registry>,
    peer: Option<i32>,
    progress: &UnboundedSender<Response>,
) -> Response {
    let jobs = &registry.jobs;
    if matches!(
        request,
        Request::Up { .. }
            | Request::Down { .. }
            | Request::Status { .. }
            | Request::Eject { .. }
            | Request::Gc { .. }
            | Request::WorktreeRelease { .. }
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
            answer(
                blocking("the job could not be started", move || {
                    start_job(&registry.jobs, command, &cwd, env, Duration::from_millis(timeout_ms))
                })
                .await,
                |(job, outcome)| Response::Run { job, outcome },
            )
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
        Request::Logs { name, cwd, env } => match jobs.get(&name) {
            Some(job) => Response::Logs { output: job.output() },
            None => service_logs(registry, name, cwd, env).await,
        },
        Request::Up {
            service,
            cwd,
            env,
            gitignore,
        } => up::up(registry, service, cwd, env, gitignore).await,
        Request::Env { service, cwd } => {
            let registry = Arc::clone(registry);
            // Resolving the Topology runs git, and reading the configuration and the Leases are blocking.
            answer(
                blocking("the environment could not be read", move || {
                    up::service_environment(&registry, &service, &cwd)
                })
                .await,
                |variables| Response::Env { variables },
            )
        }
        Request::Down { service, cwd } => answer(
            down(Arc::clone(registry), service, cwd, peer).await.map_err(message),
            |(stopped, released)| Response::Down { stopped, released },
        ),
        Request::Status { cwd, all } => {
            let registry = Arc::clone(registry);
            answer(
                blocking("the status could not be read", move || {
                    status::report(&registry.jobs, &registry.services, &registry.composes, &cwd, all)
                })
                .await,
                Response::Status,
            )
        }
        Request::WorktreeInit { path, env } => worktree_init::run(Arc::clone(registry), path, env, progress)
            .await
            .unwrap_or_else(failure),
        Request::Eject { cwd } => eject::run(registry, cwd, peer).await,
        Request::Gc { cwd, apply } => gc::run(registry, cwd, apply, peer).await,
        Request::WorktreeRelease { path } => answer(
            worktree_release::run(Arc::clone(registry), path, peer)
                .await
                .map_err(message),
            Response::WorktreeRelease,
        ),
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
        log: None,
    };
    Ok(jobs.start(&spec)?)
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
    let (released, isolated) = {
        let (registry, worktree, service) = (Arc::clone(&registry), worktree.clone(), service.clone());
        // Whether the other Worktrees still exist is read from the disk, and the record is written to it.
        tokio::task::spawn_blocking(move || {
            let composes = &registry.composes;
            let repository = &topology.git_common_dir;
            (
                composes.release(repository, &worktree, service.as_deref()),
                composes.isolated_released(repository, &worktree, service.as_deref()),
            )
        })
        .await
        .context("the containers could not be released")?
    };
    // The containers of other Worktrees, removed without `procly down`, are stopped too, but are not this one's to tell.
    let isolated: Vec<String> = registry
        .composes
        .stop_isolated(isolated)
        .await?
        .into_iter()
        .filter(|(_, _, owner)| owner.as_deref() == Some(worktree.as_path()))
        .map(|(_, name, _)| name)
        .collect();
    if let (Some(name), true) = (
        &service,
        stopping.is_empty() && released.is_empty() && isolated.is_empty(),
    ) {
        // Nothing to stop is fine for a declared Service, and a typo for any other.
        let (name, worktree, registry) = (name.clone(), worktree.clone(), Arc::clone(&registry));
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let config = read_config(&worktree)?;
            declared(&config, &worktree, &name)?;
            // Stopping the Services of a Group apart from it is not a thing, and a silent "not running" would lie.
            if let Some(group) = config.group_of(&name)
                && registry.services.runs(&worktree, group)
            {
                anyhow::bail!("{name} runs as part of group {group}, stop the group with `procly down {group}`");
            }
            Ok(())
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
    for name in isolated {
        eprintln!("down {name} of {} requested by {who}", worktree.display());
        stopped.push(name);
    }
    for released in &released {
        eprintln!("down {} of {} requested by {who}", released.service, worktree.display());
    }
    Ok((stopped, released))
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

/// The recent output of the Service `name` of the Worktree holding `cwd`, once the Services of the previous Daemon
/// are reconciled, so that a re-attached Service shows its output from before the restart. The output of a Compose
/// service is the one of its container, which `docker compose logs` reads in `env`.
async fn service_logs(registry: &Registry, name: String, cwd: PathBuf, env: Vec<(String, String)>) -> Response {
    if let Err(message) = registry.reconciled().await {
        return Response::Error { message };
    }
    let unknown = || Response::Error {
        message: format!("no job or service {name}"),
    };
    // Outside a git Repository there is no Service, and the name is simply unknown.
    let Ok(Ok(topology)) = tokio::task::spawn_blocking(move || topology_of(&cwd)).await else {
        return unknown();
    };
    if let Some(output) = registry.services.output_of(&topology.worktree.path, &name) {
        return Response::Logs { output };
    }
    let compose = {
        let name = name.clone();
        // Reading the configuration is blocking, and finding the Main worktree runs git.
        blocking("the logs could not be read", move || {
            let scope = up::Scope::of(&topology.worktree.path)?;
            match scope
                .config()
                .services
                .get(&name)
                .map(|declared| declared.kind(&name))
                .transpose()?
            {
                Some(config::ServiceKind::Compose { service }) => {
                    let (project_dir, isolated) = scope.project(&name)?;
                    Ok(Some((service.to_owned(), project_dir, isolated)))
                }
                Some(config::ServiceKind::Process { .. } | config::ServiceKind::Group { .. }) | None => Ok(None),
            }
        })
        .await
    };
    match compose {
        Ok(Some((compose, project_dir, isolated))) => registry
            .composes
            .logs(&name, &compose, project_dir, isolated.as_ref(), env)
            .await
            .map_or_else(failure, |output| Response::Logs { output }),
        Ok(None) => unknown(),
        Err(message) => Response::Error { message },
    }
}

/// Runs `work` on a blocking thread, as git, the file system and forks need, and turns its failure into the message
/// of an error answer. `failed` says what could not be done when the work panicked.
async fn blocking<T: Send + 'static>(
    failed: &'static str,
    work: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
) -> Result<T, String> {
    match tokio::task::spawn_blocking(work).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(message(error)),
        Err(error) => Err(format!("{failed}: {error}")),
    }
}

/// The answer to a request that went through, or the error answer carrying the message it failed with.
fn answer<T>(result: Result<T, String>, answer: impl FnOnce(T) -> Response) -> Response {
    result.map_or_else(|message| Response::Error { message }, answer)
}

/// The error answer telling `error` with its causes.
fn failure(error: impl Into<anyhow::Error>) -> Response {
    Response::Error {
        message: message(error),
    }
}

/// The message of `error` followed by its causes, as an error answer carries it.
fn message(error: impl Into<anyhow::Error>) -> String {
    format!("{:#}", error.into())
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
