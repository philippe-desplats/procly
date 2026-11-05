//! Restart: a Service stopped and started again with the environment of its last `up` (ADR 0014).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::up::{self, Scope};
use super::{Registry, blocking, down, failure, requester};
use crate::protocol::Response;

/// What `run` says when its blocking step could not run to its end.
const RESTART_FAILED: &str = "the service could not be restarted";

/// What a Restart brings up again, worked out before anything is stopped.
struct Planned {
    scope: Scope,
    /// The Service that is stopped and started again: the one asked for, or the running Group covering it.
    target: String,
    /// The start order of `target`, its needs first.
    order: Vec<String>,
    /// The environment of the last `up` of `target`.
    env: Vec<(String, String)>,
}

/// Restarts the Service `service` of the Worktree holding `cwd`, logging who asked: stops it if it runs, through the
/// path of `down`, then brings it up again with the environment of its last `up` and the same Lease, the Services it
/// needs that do not run included, and answers as `up` does for one Service. A Service covered by a Group that runs is
/// restarted through its Group. Without a held environment, nothing is stopped and the answer is an error.
pub(super) async fn run(registry: &Arc<Registry>, service: String, cwd: PathBuf, peer: Option<i32>) -> Response {
    let planned = {
        let registry = Arc::clone(registry);
        // Resolving the Topology runs git, reading the configuration is blocking, and naming the requester runs ps.
        blocking(RESTART_FAILED, move || {
            let planned = plan(&registry, &service, &cwd)?;
            let who = requester::describe(peer, &cwd);
            eprintln!(
                "restart {} of {} requested by {who}",
                planned.target,
                planned.scope.worktree().display()
            );
            Ok(planned)
        })
        .await
    };
    let Planned {
        scope,
        target,
        order,
        env,
    } = match planned {
        Ok(planned) => planned,
        Err(message) => return Response::Error { message },
    };
    if let Err(error) = down(
        Arc::clone(registry),
        Some(target.clone()),
        scope.worktree().to_owned(),
        peer,
    )
    .await
    {
        return failure(error);
    }
    up::reach(registry, &scope, Some(target), &order, &env, false).await
}

/// The Service a Restart of `service` stops and starts again, its start order and the environment it is given.
/// Blocking.
fn plan(registry: &Registry, service: &str, cwd: &Path) -> anyhow::Result<Planned> {
    let scope = Scope::of(cwd)?;
    scope.declared(service)?;
    let worktree = scope.worktree();
    let target = match scope.config().group_of(service) {
        Some(group) if registry.services.runs(worktree, group) => group.to_owned(),
        Some(_) | None => service.to_owned(),
    };
    let Some(env) = registry.held.get(worktree, &target) else {
        anyhow::bail!(
            "{service} has no environment to restart with, run procly up {service} from a shell in the Worktree"
        );
    };
    let order = scope.start_order(Some(&target))?;
    Ok(Planned {
        scope,
        target,
        order,
        env,
    })
}
