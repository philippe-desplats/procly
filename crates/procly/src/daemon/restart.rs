//! Restart: a Service stopped and started again with the environment of its last `up` (ADR 0014).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::held::Admission;
use super::up::{self, Scope};
use super::{Registry, blocking, down, failure, requester};
use crate::protocol::Response;

/// What `run` says when its blocking step could not run to its end.
const RESTART_FAILED: &str = "the service could not be restarted";

/// What a Restart brings up again, worked out before anything is stopped.
struct Planned {
    scope: Scope,
    /// The Service that is stopped and started again: the one asked for, or the Group covering it.
    target: String,
    /// The start order of `target`, its needs first.
    order: Vec<String>,
}

/// Restarts the Service `service` of the Worktree holding `cwd`, logging who asked: stops it if it runs, through the
/// path of `down`, then brings it up again with the environment of its last `up` and the same Lease, the Services it
/// needs that do not run included, and answers as `up` does for one Service. A Service a Group covers is restarted
/// through its Group, unless it runs on its own, which its Group cannot start over. Without a held environment, nothing
/// is stopped and the answer is an error. A Worktree
/// release that begins once the environment is read wins: the Restart starts nothing more and answers an error.
pub(super) async fn run(registry: &Arc<Registry>, service: String, cwd: PathBuf, peer: Option<i32>) -> Response {
    let planned = {
        let (registry, service) = (Arc::clone(registry), service.clone());
        // Resolving the Topology runs git, reading the configuration is blocking, and naming the requester runs ps.
        blocking(RESTART_FAILED, move || {
            Ok((plan(&registry, &service, &cwd)?, requester::describe(peer, &cwd)))
        })
        .await
    };
    let (Planned { scope, target, order }, who) = match planned {
        Ok(planned) => planned,
        Err(message) => return Response::Error { message },
    };
    let worktree = scope.worktree().to_owned();
    // A Service the running Group was found covering by an `up` holds the environment of that `up`, the Group may hold
    // none, as when a previous Daemon started it.
    let Some((env, admission)) = registry
        .held
        .admission(&worktree, &[target.as_str(), service.as_str()])
        .await
    else {
        return Response::Error {
            message: format!(
                "{service} has no environment to restart with, run procly up {service} from a shell in the Worktree"
            ),
        };
    };
    eprintln!("restart {target} of {} requested by {who}", worktree.display());
    if let Err(error) = down(Arc::clone(registry), Some(target.clone()), worktree, peer).await {
        return failure(error);
    }
    let reached = up::reach(
        registry,
        &scope,
        Some(target.clone()),
        &order,
        &env,
        false,
        Some(&admission),
    )
    .await;
    // What started before the release began is the release's to stop, the Restart did not bring the Service back.
    if admission.holds().await {
        reached
    } else {
        refused(&admission, &target)
    }
}

/// The error answer of a Restart of `target` that a Worktree release overtook.
pub(super) fn refused(admission: &Admission, target: &str) -> Response {
    Response::Error {
        message: admission.refusal(target),
    }
}

/// The Service a Restart of `service` stops and starts again and its start order. Blocking.
fn plan(registry: &Registry, service: &str, cwd: &Path) -> anyhow::Result<Planned> {
    let scope = Scope::of(cwd)?;
    scope.declared(service)?;
    let worktree = scope.worktree();
    // A covered Service that runs on its own is the one thing its Group cannot start over, so it is restarted alone.
    let target = match scope.config().group_of(service) {
        Some(group) if !registry.services.runs(worktree, service) => group.to_owned(),
        Some(_) | None => service.to_owned(),
    };
    let order = scope.start_order(Some(&target))?;
    // Before anything is stopped, so that a start refused for a declared port leaves the Service running on its Lease.
    let mut stopping = vec![target.clone()];
    stopping.extend(
        scope
            .config()
            .services
            .get(&target)
            .and_then(|declared| declared.covers.clone())
            .unwrap_or_default(),
    );
    scope.rehearse_start(registry, &order, &stopping)?;
    Ok(Planned { scope, target, order })
}
