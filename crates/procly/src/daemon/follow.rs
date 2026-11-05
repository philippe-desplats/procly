//! A followed `Logs`: the output of a Job or a Process service streamed as it arrives, until it ends.
//!
//! The output reaches the connection through the bounded queue of an [`output::Follow`], so that a reader too slow
//! never holds the Job or the Service that prints, and misses output instead, which it is told about.

use std::path::PathBuf;
use std::sync::Arc;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

use super::job::Job;
use super::service::Service;
use super::{Registry, blocking, compose_service, topology_of};
use crate::output::{self, Chunk, Text};
use crate::protocol::{self, Response};

/// What a follow waits for the end of.
enum Followed {
    Job(Arc<Job>),
    Service(Arc<Service>),
}

impl Followed {
    async fn ended(&self) {
        match self {
            Self::Job(job) => {
                job.ended().await;
            }
            // Only an ended Service ends the follow, one restarting comes back with more output.
            Self::Service(service) => service.ended().await,
        }
    }
}

/// Streams the output of the Job `name`, or else of the Service `name` of the Worktree holding `cwd`, on the
/// connection, until it ends or the client closes the connection.
pub(super) async fn run(
    registry: &Arc<Registry>,
    name: String,
    cwd: PathBuf,
    env: Vec<(String, String)>,
    mut reader: BufReader<OwnedReadHalf>,
    mut writer: OwnedWriteHalf,
) {
    let (followed, mut follow) = match find(registry, &name, cwd, env).await {
        Ok(found) => found,
        Err(message) => {
            let _ = send(&mut writer, Response::Error { message }).await;
            return;
        }
    };
    let backlog = std::mem::take(&mut follow.backlog);
    if send(&mut writer, Response::Logs { output: backlog }).await.is_err() {
        return;
    }
    let mut text = std::mem::take(&mut follow.text);
    let ended = followed.ended();
    tokio::pin!(ended);
    let mut probe = [0; 1];
    loop {
        tokio::select! {
            biased;
            chunk = follow.next() => {
                let Some(chunk) = chunk else { break };
                if forward(&mut writer, &mut text, chunk).await.is_err() {
                    return;
                }
            }
            () = &mut ended => break,
            // The client sends nothing after its request, so anything read here means it is gone: closing the
            // connection, or only its writing half, cancels the follow.
            _ = reader.read(&mut probe) => return,
        }
    }
    // The Job and the Service push their last output before they are told ended, it is all queued by now.
    let (queued, dropped) = follow.finish();
    for chunk in queued {
        if forward(&mut writer, &mut text, chunk).await.is_err() {
            return;
        }
    }
    let rest = text.finish();
    if !rest.is_empty() && send(&mut writer, Response::Output { text: rest }).await.is_err() {
        return;
    }
    if dropped > 0
        && send(&mut writer, Response::OutputDropped { bytes: dropped })
            .await
            .is_err()
    {
        return;
    }
    let _ = send(&mut writer, Response::LogsEnded).await;
}

/// The Job or the Service `name` and the follow of its output, or the message of the error answer.
async fn find(
    registry: &Arc<Registry>,
    name: &str,
    cwd: PathBuf,
    env: Vec<(String, String)>,
) -> Result<(Followed, output::Follow), String> {
    if let Some(job) = registry.jobs.get(name) {
        let follow = job.follow();
        return Ok((Followed::Job(job), follow));
    }
    // A re-attached Service shows its output from before the restart of the Daemon.
    registry.reconciled().await?;
    let unknown = || format!("no job or service {name}");
    // Outside a git Repository there is no Service, and the name is simply unknown.
    let Ok(Ok(topology)) = tokio::task::spawn_blocking(move || topology_of(&cwd)).await else {
        return Err(unknown());
    };
    let worktree = topology.worktree.path;
    if let Some((service, follow)) = registry.services.follow(&worktree, name) {
        return Ok((Followed::Service(service), follow));
    }
    let compose = {
        let name = name.to_owned();
        blocking("the logs could not be read", move || compose_service(&worktree, &name)).await?
    };
    let Some((compose, project_dir, isolated)) = compose else {
        return Err(unknown());
    };
    let command = registry
        .composes
        .follow_command(name, &compose, project_dir, isolated.as_ref(), env)
        .await
        .map_err(super::message)?;
    Err(format!(
        "{name} is a Compose service, whose output procly cannot follow, follow it with `{command}`"
    ))
}

/// Sends what the client missed before `chunk`, then `chunk`.
async fn forward(writer: &mut OwnedWriteHalf, text: &mut Text, chunk: Chunk) -> std::io::Result<()> {
    if chunk.dropped > 0 {
        // What was cut in half before the gap never gets its other half.
        let rest = text.finish();
        if !rest.is_empty() {
            send(writer, Response::Output { text: rest }).await?;
        }
        send(writer, Response::OutputDropped { bytes: chunk.dropped }).await?;
    }
    let decoded = text.decode(&chunk.bytes);
    if decoded.is_empty() {
        return Ok(());
    }
    send(writer, Response::Output { text: decoded }).await
}

/// Writes one line to the client, failing once it is gone.
async fn send(writer: &mut OwnedWriteHalf, response: Response) -> std::io::Result<()> {
    let line = protocol::encode(response).expect("the answers of a follow hold only strings and numbers");
    writer.write_all(line.as_bytes()).await
}
