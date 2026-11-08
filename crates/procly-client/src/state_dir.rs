//! The per-user state directory holding the Daemon socket, its lock, pid file and log, and its persistent state.

use std::fs::{self, DirBuilder, Permissions};
use std::io;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::PathBuf;

/// Overrides the state directory, used by tests to run an isolated Daemon.
pub const STATE_DIR_ENV: &str = "PROCLY_STATE_DIR";

#[derive(Debug, thiserror::Error)]
pub enum StateDirError {
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
pub struct StateDir {
    path: PathBuf,
}

impl StateDir {
    /// `$PROCLY_STATE_DIR`, then `$XDG_STATE_HOME/procly`, then `~/.local/state/procly`.
    ///
    /// # Errors
    ///
    /// When none of these variables is set, or the directory they name cannot be made absolute.
    pub fn resolve() -> Result<Self, StateDirError> {
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

    /// The state directory at `path`, taken as given, for a caller that already knows where it is.
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Creates the directory readable by the user only, so the socket inside it is never reachable by others.
    ///
    /// # Errors
    ///
    /// When the directory cannot be created or its permissions set.
    pub fn create(&self) -> Result<(), StateDirError> {
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

    #[must_use]
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    #[must_use]
    pub fn socket(&self) -> PathBuf {
        self.path.join("procly.sock")
    }

    #[must_use]
    pub fn lock(&self) -> PathBuf {
        self.path.join("daemon.lock")
    }

    #[must_use]
    pub fn pid(&self) -> PathBuf {
        self.path.join("daemon.pid")
    }

    #[must_use]
    pub fn log(&self) -> PathBuf {
        self.path.join("daemon.log")
    }

    /// The document holding every Lease.
    #[must_use]
    pub fn leases(&self) -> PathBuf {
        self.path.join("leases.json")
    }

    /// The document holding the index of every Logical resource a Driver numbers.
    #[must_use]
    pub fn indexes(&self) -> PathBuf {
        self.path.join("indexes.json")
    }

    /// The document recording the Logical resources of every Worktree other than the Main worktree, which `procly gc`
    /// drops once the Worktree is removed.
    #[must_use]
    pub fn resources(&self) -> PathBuf {
        self.path.join("resources.json")
    }

    /// The document recording every running Service, which the next Daemon reconciles after a crash.
    #[must_use]
    pub fn services(&self) -> PathBuf {
        self.path.join("services.json")
    }

    /// The document recording every container Procly started or adopted, with the Worktrees that need it, which the
    /// next Daemon reconciles after a crash or a restart.
    #[must_use]
    pub fn containers(&self) -> PathBuf {
        self.path.join("containers.json")
    }

    /// The document recording every Isolated container Procly started or adopted, running or stopped, until `procly
    /// gc --apply` removes it.
    #[must_use]
    pub fn isolated(&self) -> PathBuf {
        self.path.join("isolated.json")
    }

    /// The document recording every Portless alias Procly made, which goes with the Lease it names.
    #[must_use]
    pub fn portless_aliases(&self) -> PathBuf {
        self.path.join("portless-aliases.json")
    }

    /// The directory holding the output file of every running Service, which outlives a crash of the Daemon.
    #[must_use]
    pub fn outputs(&self) -> PathBuf {
        self.path.join("output")
    }

    /// The copy of its own executable that the Daemon makes when it starts and runs its helpers from, so that an
    /// upgrade replacing or removing the executable it was started from after that copy changes nothing for its helpers.
    #[must_use]
    pub fn executable(&self) -> PathBuf {
        self.path.join("daemon-executable")
    }

    /// The directory holding the log of the setup commands of each Worktree, which `worktree init` removes once they all
    /// succeeded.
    #[must_use]
    pub fn setup_logs(&self) -> PathBuf {
        self.path.join("setup")
    }

    /// The document listing the Repositories that are Off.
    #[must_use]
    pub fn off(&self) -> PathBuf {
        self.path.join("off.json")
    }

    /// The document holding the fingerprint of every Managed block Procly wrote.
    #[must_use]
    pub fn managed_blocks(&self) -> PathBuf {
        self.path.join("managed-blocks.json")
    }
}

fn non_empty_env(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}
