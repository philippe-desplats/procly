//! The per-user state directory holding the Daemon socket, its lock, pid file and log, and its persistent state.

use std::fs::{self, DirBuilder, Permissions};
use std::io;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::PathBuf;

/// Overrides the state directory, used by tests to run an isolated Daemon.
pub(crate) const STATE_DIR_ENV: &str = "PROCLY_STATE_DIR";

#[derive(Debug, thiserror::Error)]
pub(crate) enum StateDirError {
    #[error("cannot locate the state directory: set {STATE_DIR_ENV}, XDG_STATE_HOME or HOME")]
    Unresolved,
    #[error("cannot resolve the state directory {path}")]
    Absolute {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cannot create the state directory {path}")]
    Create {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct StateDir {
    path: PathBuf,
}

impl StateDir {
    /// `$PROCLY_STATE_DIR`, then `$XDG_STATE_HOME/procly`, then `~/.local/state/procly`.
    pub(crate) fn resolve() -> Result<Self, StateDirError> {
        let path = if let Some(dir) = non_empty_env(STATE_DIR_ENV) {
            dir
        } else if let Some(dir) = non_empty_env("XDG_STATE_HOME") {
            dir.join("procly")
        } else if let Some(home) = non_empty_env("HOME") {
            home.join(".local/state/procly")
        } else {
            return Err(StateDirError::Unresolved);
        };
        // The Daemon runs from `/`, so a relative path must be fixed before it is handed over.
        let path = std::path::absolute(&path).map_err(|source| StateDirError::Absolute { path, source })?;
        Ok(Self { path })
    }

    /// Creates the directory readable by the user only, so the socket inside it is never reachable by others.
    pub(crate) fn create(&self) -> Result<(), StateDirError> {
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.path)
            .and_then(|()| fs::set_permissions(&self.path, Permissions::from_mode(0o700)))
            .map_err(|source| StateDirError::Create {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) fn socket(&self) -> PathBuf {
        self.path.join("procly.sock")
    }

    pub(crate) fn lock(&self) -> PathBuf {
        self.path.join("daemon.lock")
    }

    pub(crate) fn pid(&self) -> PathBuf {
        self.path.join("daemon.pid")
    }

    pub(crate) fn log(&self) -> PathBuf {
        self.path.join("daemon.log")
    }

    /// The document holding every Lease.
    pub(crate) fn leases(&self) -> PathBuf {
        self.path.join("leases.json")
    }

    /// The document recording every running Service, which the next Daemon reconciles after a crash.
    pub(crate) fn services(&self) -> PathBuf {
        self.path.join("services.json")
    }
}

fn non_empty_env(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}
