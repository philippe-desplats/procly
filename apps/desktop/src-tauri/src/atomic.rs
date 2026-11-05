//! Files the app keeps for itself (`seen.json`, `settings.json`) are replaced whole, never edited in place.

use std::path::{Path, PathBuf};
use std::{fs, io};

/// Writes `bytes` to `path` so that a reader sees the old file or the new one, never half of it. The directory of the
/// file is created when it is missing.
///
/// # Errors
///
/// When the directory cannot be created, or the file cannot be written or put in place.
pub(crate) fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut pending = path.as_os_str().to_owned();
    pending.push(".tmp");
    let pending = PathBuf::from(pending);
    fs::write(&pending, bytes)?;
    fs::rename(&pending, path)
}
