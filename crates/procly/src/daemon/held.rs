//! The environment of the last `up` of each Service, which a Restart replays (ADR 0014).
//!
//! Held in memory only and never written to disk nor returned by the status, since it may carry secrets. It is gone
//! when the Daemon stops, so a Service a previous Daemon started has none.
//!
//! A Worktree release drops the environments of its Worktree under the release gate of that Worktree, which a Restart
//! holds while it starts a Service or reads a Lease, so that a release that began during a Restart wins: the Restart
//! starts nothing and takes back no Lease once the release has begun.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use tokio::sync::OwnedMutexGuard;

/// The variables of an environment, in the order the caller gave them.
type Environment = Vec<(String, String)>;

/// A Service, by the root of its Worktree and its name.
type ServiceKey = (PathBuf, String);

/// How many Worktree releases a Worktree went through, locked by a release to count one more and by a Restart to
/// start a Service while there is none it has not seen.
type ReleaseGate = Arc<tokio::sync::Mutex<u64>>;

/// The environment of the last `up` of each Service.
#[derive(Debug, Default)]
pub(super) struct HeldEnvironments {
    table: Mutex<HashMap<ServiceKey, Environment>>,
    gates: Mutex<HashMap<PathBuf, ReleaseGate>>,
}

/// What lets a Restart start the Services of its Worktree: the count of releases of the Worktree as the Restart read
/// the environment it starts them with.
#[derive(Debug)]
pub(super) struct Admission {
    gate: ReleaseGate,
    releases: u64,
    worktree: PathBuf,
}

impl Admission {
    /// Locks the release gate of the Worktree, `None` when a release has begun since the environment was read. A
    /// release waits for the guard, so that what is started under it is in place for the release to stop.
    pub(super) async fn admit(&self) -> Option<OwnedMutexGuard<u64>> {
        let guard = Arc::clone(&self.gate).lock_owned().await;
        (*guard == self.releases).then_some(guard)
    }

    /// Whether no release of the Worktree has begun since the environment was read.
    pub(super) async fn holds(&self) -> bool {
        *self.gate.lock().await == self.releases
    }

    /// Why the Restart of `service` starts nothing.
    pub(super) fn refusal(&self, service: &str) -> String {
        format!(
            "the Worktree {} was released while {service} was restarting, nothing was started again",
            self.worktree.display()
        )
    }
}

impl HeldEnvironments {
    /// Holds `env` as the environment of the last `up` of the Service `name` of `worktree`, in place of the one held.
    pub(super) fn hold(&self, worktree: &Path, name: &str, env: &[(String, String)]) {
        self.table()
            .insert((worktree.to_owned(), name.to_owned()), env.to_owned());
    }

    /// The environment held for the Service `name` of `worktree`, `None` when no `up` of this Daemon gave one.
    pub(super) fn get(&self, worktree: &Path, name: &str) -> Option<Environment> {
        self.table().get(&(worktree.to_owned(), name.to_owned())).cloned()
    }

    /// Whether an environment is held for the Service `name` of `worktree`.
    pub(super) fn holds(&self, worktree: &Path, name: &str) -> bool {
        self.table().contains_key(&(worktree.to_owned(), name.to_owned()))
    }

    /// The environment held for the first of `names` that has one in `worktree`, with what lets a Restart start the
    /// Services of `worktree` in it, both read under the release gate so that no release falls between them.
    pub(super) async fn admission(&self, worktree: &Path, names: &[&str]) -> Option<(Environment, Admission)> {
        let gate = self.gate(worktree);
        let releases = *gate.lock().await;
        let env = names.iter().find_map(|name| self.get(worktree, name))?;
        Some((
            env,
            Admission {
                gate,
                releases,
                worktree: worktree.to_owned(),
            },
        ))
    }

    /// Counts one more release of `worktree` and drops the environment of every Service of it, once no Restart is
    /// starting a Service there. From then on, a Restart that read its environment before starts nothing.
    pub(super) async fn release(&self, worktree: &Path) {
        let gate = self.gate(worktree);
        let mut releases = gate.lock().await;
        *releases += 1;
        self.table().retain(|(path, _), _| path != worktree);
    }

    fn gate(&self, worktree: &Path) -> ReleaseGate {
        let mut gates = self
            .gates
            .lock()
            .expect("the release gates mutex is never poisoned, no code panics while holding it");
        Arc::clone(gates.entry(worktree.to_owned()).or_default())
    }

    fn table(&self) -> MutexGuard<'_, HashMap<ServiceKey, Environment>> {
        self.table
            .lock()
            .expect("the held environments mutex is never poisoned, no code panics while holding it")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(value: &str) -> Environment {
        vec![("PATH".to_owned(), value.to_owned())]
    }

    #[test]
    fn every_up_replaces_the_environment_held_for_its_service() {
        let held = HeldEnvironments::default();
        let app = Path::new("/src/app");
        assert!(!held.holds(app, "web"));

        held.hold(app, "web", &env("/first"));
        held.hold(app, "web", &env("/second"));

        assert_eq!(held.get(app, "web"), Some(env("/second")));
        assert!(held.holds(app, "web"));
        assert_eq!(held.get(app, "api"), None);
    }

    #[tokio::test]
    async fn a_worktree_release_drops_the_environments_of_that_worktree_only() {
        let held = HeldEnvironments::default();
        let (app, feature) = (Path::new("/src/app"), Path::new("/src/app-feature"));
        held.hold(app, "web", &env("/app"));
        held.hold(feature, "web", &env("/feature"));
        held.hold(feature, "db", &env("/feature"));

        held.release(feature).await;

        assert_eq!(held.get(app, "web"), Some(env("/app")));
        assert!(!held.holds(feature, "web"));
        assert!(!held.holds(feature, "db"));
    }

    #[tokio::test]
    async fn a_release_after_the_environment_was_read_refuses_every_later_start_in_that_worktree_only() {
        let held = HeldEnvironments::default();
        let (app, feature) = (Path::new("/src/app"), Path::new("/src/app-feature"));
        held.hold(app, "web", &env("/app"));
        held.hold(feature, "web", &env("/feature"));
        let (_, in_app) = held.admission(app, &["web"]).await.unwrap();
        let (env_read, in_feature) = held.admission(feature, &["api", "web"]).await.unwrap();
        assert_eq!(env_read, env("/feature"));
        assert!(in_feature.admit().await.is_some());

        held.release(feature).await;

        assert!(in_feature.admit().await.is_none());
        assert!(!in_feature.holds().await);
        assert!(in_app.admit().await.is_some());
        assert!(held.admission(feature, &["web"]).await.is_none());
    }
}
