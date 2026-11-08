//! The executable the Daemon runs its helpers from: a copy of its own, made when it starts.
//!
//! A path names whatever file is there when it is used. On macOS `std::env::current_exe` answers the path the process
//! was started from, so a Daemon that outlives an upgrade, one that puts a new file at that path as a cask does or
//! removes the directory as `brew upgrade` does for the formula, would run the new executable as its helper, or none.
//! Its helpers must match the Daemon that starts them, so the Daemon copies its executable into its state directory
//! once, before it serves anything, and starts every helper from the copy. The copy is a new file for each Daemon,
//! never written once it is there, so that a helper still running from the copy of a previous Daemon is left alone.
//!
//! A link would be cheaper but is not safe: on macOS, running a file while another name of it is linked or unlinked
//! kills the process with `SIGKILL`, measured at 11 runs in 3202. The first run of a new file takes about half a second
//! on macOS, where the system assesses it, which would delay the writer of the first Service so that its output
//! reaches `procly up` late. The Daemon therefore runs the copy once, before it accepts a request, to pay that time
//! here.

use std::fs::{self, Permissions};
use std::io;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context as _, bail};

use crate::state_dir::StateDir;

/// Copies the executable this Daemon runs to the pinned executable of `state`, and returns its path.
pub(crate) fn pin(state: &StateDir) -> anyhow::Result<PathBuf> {
    let source = std::env::current_exe().context("cannot locate the executable this daemon runs")?;
    let pinned = state.executable();
    copy(&source, &pinned).with_context(|| format!("cannot copy {} to {}", source.display(), pinned.display()))?;
    Ok(pinned)
}

/// Runs the pinned executable `executable` once, on a blocking thread. The Daemon does it with its socket bound and
/// before it accepts a request, so that a command waits in the queue of the socket and never fails to connect.
pub(crate) async fn warm(executable: PathBuf) -> anyhow::Result<()> {
    tokio::task::spawn_blocking(move || {
        run_once(&executable).with_context(|| format!("cannot run the copy {}", executable.display()))
    })
    .await
    .context("the run of the copy panicked")?
}

/// Copies `source` to `destination` as a new file, replacing the file there, so that whoever runs the file it
/// replaced keeps running it. Only the user can run it.
fn copy(source: &Path, destination: &Path) -> io::Result<()> {
    let staged = destination.with_extension("new");
    let copied = fs::copy(source, &staged)
        .and_then(|_| fs::set_permissions(&staged, Permissions::from_mode(0o700)))
        .and_then(|()| fs::rename(&staged, destination));
    if copied.is_err() {
        let _ = fs::remove_file(&staged);
    }
    copied
}

/// Runs `executable` for its version, which it prints nowhere.
fn run_once(executable: &Path) -> anyhow::Result<()> {
    let status = Command::new(executable)
        .arg("--version")
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if !status.success() {
        bail!("it ended with {status}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::MetadataExt as _;

    use super::*;

    #[test]
    fn copies_the_file_as_a_new_one_for_the_user_only_and_leaves_the_file_it_replaced_to_whoever_runs_it() {
        let dir = tempfile::tempdir().unwrap();
        let (source, destination) = (dir.path().join("source"), dir.path().join("daemon-executable"));
        fs::write(&source, "new").unwrap();
        fs::set_permissions(&source, Permissions::from_mode(0o755)).unwrap();
        fs::write(&destination, "old").unwrap();
        let old = fs::File::open(&destination).unwrap();

        copy(&source, &destination).unwrap();

        assert_eq!(fs::read_to_string(&destination).unwrap(), "new");
        assert_eq!(fs::metadata(&destination).unwrap().permissions().mode() & 0o777, 0o700);
        assert_ne!(
            fs::metadata(&source).unwrap().ino(),
            fs::metadata(&destination).unwrap().ino()
        );
        assert_eq!(io::read_to_string(&old).unwrap(), "old");
        assert!(!dir.path().join("daemon-executable.new").exists());
    }

    #[test]
    fn leaves_the_pinned_file_and_nothing_else_when_the_source_cannot_be_copied() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("daemon-executable");
        fs::write(&destination, "old").unwrap();

        assert!(copy(&dir.path().join("missing"), &destination).is_err());

        assert_eq!(fs::read_to_string(&destination).unwrap(), "old");
        assert!(!dir.path().join("daemon-executable.new").exists());
    }

    #[test]
    fn runs_the_executable_it_is_given_and_reports_one_that_cannot_run_or_fails() {
        let dir = tempfile::tempdir().unwrap();

        assert!(run_once(Path::new("/usr/bin/true")).is_ok());
        assert!(run_once(Path::new("/usr/bin/false")).is_err());
        assert!(run_once(&dir.path().join("missing")).is_err());
    }
}
