//! The Daemon: the single per-user authority, reachable only through a `0600` Unix socket (ADR 0003).

mod job;

use std::fs::{self, File, Permissions, TryLockError};
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::signal::unix::{SignalKind, signal};

use self::job::{JobSpec, Jobs};
use crate::protocol::{self, Request, Response};
use crate::state_dir::StateDir;

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

    let jobs = Arc::new(Jobs::default());
    let mut terminate = signal(SignalKind::terminate()).context("cannot handle SIGTERM")?;
    let mut interrupt = signal(SignalKind::interrupt()).context("cannot handle SIGINT")?;
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    tokio::spawn(handle(stream, Arc::clone(&jobs)));
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
    jobs.kill_all();
    remove_if_present(&socket)?;
    remove_if_present(&state.pid())?;
    Ok(())
}

async fn handle(stream: UnixStream, jobs: Arc<Jobs>) {
    let (reader, mut writer) = stream.into_split();
    let mut line = String::new();
    let response = match BufReader::new(reader).read_line(&mut line).await {
        Ok(_) => match protocol::decode::<Request>(&line) {
            Ok(request) => dispatch(request, &jobs).await,
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
    let _ = writer.write_all(protocol::encode(response).as_bytes()).await;
}

async fn dispatch(request: Request, jobs: &Arc<Jobs>) -> Response {
    match request {
        Request::Run {
            command,
            cwd,
            env,
            timeout_ms,
        } => {
            let spec = JobSpec {
                command,
                cwd,
                env,
                timeout: Duration::from_millis(timeout_ms),
            };
            let jobs = Arc::clone(jobs);
            // Opening a terminal and forking are blocking system calls.
            match tokio::task::spawn_blocking(move || jobs.start(&spec)).await {
                Ok(Ok(job)) => Response::Started { job },
                Ok(Err(error)) => Response::Error {
                    message: format!("{:#}", anyhow::Error::from(error)),
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
