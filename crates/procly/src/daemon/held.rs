//! The environment of the last `up` of each Service, which a Restart replays (ADR 0014).
//!
//! Held in memory only and never written to disk nor returned by the status, since it may carry secrets. It is gone
//! when the Daemon stops, so a Service a previous Daemon started has none.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

/// The variables of an environment, in the order the caller gave them.
type Environment = Vec<(String, String)>;

/// A Service, by the root of its Worktree and its name.
type ServiceKey = (PathBuf, String);

/// The environment of the last `up` of each Service.
#[derive(Debug, Default)]
pub(super) struct HeldEnvironments {
    table: Mutex<HashMap<ServiceKey, Environment>>,
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

    /// Drops the environment of every Service of `worktree`, as its Worktree release does.
    pub(super) fn drop_worktree(&self, worktree: &Path) {
        self.table().retain(|(path, _), _| path != worktree);
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

    #[test]
    fn a_worktree_release_drops_the_environments_of_that_worktree_only() {
        let held = HeldEnvironments::default();
        let (app, feature) = (Path::new("/src/app"), Path::new("/src/app-feature"));
        held.hold(app, "web", &env("/app"));
        held.hold(feature, "web", &env("/feature"));
        held.hold(feature, "db", &env("/feature"));

        held.drop_worktree(feature);

        assert_eq!(held.get(app, "web"), Some(env("/app")));
        assert!(!held.holds(feature, "web"));
        assert!(!held.holds(feature, "db"));
    }
}
