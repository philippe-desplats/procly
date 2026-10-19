//! Replacing a file in one step: written next to it, flushed to disk, then renamed over it, so that a crash leaves
//! either the previous content or the new one, never half of it.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions, Permissions};
use std::io::{self, Write as _};
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

/// The permissions of a replaced file.
#[derive(Debug)]
pub(crate) enum Mode {
    /// Readable and writable by the user only, for a file that may carry a credential or the state of the Daemon.
    Private,
    /// The permissions of the file it replaces, or the default ones of a new file, for a file the user owns.
    Kept,
    /// These permissions, whatever the file had.
    Exactly(Permissions),
}

/// Replaces `path` with `bytes`, see the module.
pub(crate) fn replace(path: &Path, bytes: &[u8], mode: Mode) -> io::Result<()> {
    replace_with(path, mode, |file| file.write_all(bytes))
}

/// Replaces `path` with what `write` writes into the file it is given, see the module. The partial file is removed
/// when any step fails.
pub(crate) fn replace_with(path: &Path, mode: Mode, write: impl FnOnce(&mut File) -> io::Result<()>) -> io::Result<()> {
    let partial = partial(path);
    let replaced = write_partial(path, &partial, mode, write).and_then(|()| fs::rename(&partial, path));
    if replaced.is_err() {
        let _ = fs::remove_file(&partial);
    }
    replaced?;
    // The rename itself is durable only once the directory holding it is.
    match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => File::open(dir)?.sync_all(),
        _ => Ok(()),
    }
}

fn write_partial(
    path: &Path,
    partial: &Path,
    mode: Mode,
    write: impl FnOnce(&mut File) -> io::Result<()>,
) -> io::Result<()> {
    let permissions = match mode {
        Mode::Private => Some(Permissions::from_mode(0o600)),
        Mode::Kept => fs::metadata(path).ok().map(|metadata| metadata.permissions()),
        Mode::Exactly(permissions) => Some(permissions),
    };
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    if permissions.is_some() {
        // Never readable by others before its permissions are set, whatever they are to be.
        options.mode(0o600);
    }
    let mut file = options.open(partial)?;
    write(&mut file)?;
    if let Some(permissions) = permissions {
        file.set_permissions(permissions)?;
    }
    file.sync_all()
}

/// Where `path` is written before it is renamed over it, next to it so that the rename never crosses a file system.
fn partial(path: &Path) -> PathBuf {
    let mut partial = OsString::from(path.as_os_str());
    partial.push(".procly-partial");
    PathBuf::from(partial)
}
