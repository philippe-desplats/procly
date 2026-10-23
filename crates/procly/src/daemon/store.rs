//! The persistent state of the Daemon: one versioned JSON document per concern in the state directory (ADR 0012).
//!
//! The Daemon is the only writer, it holds the lock of the state directory, so a document needs no locking of its
//! own. Each one is replaced atomically, a crash leaves either the previous content or the new one.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use super::atomic_file::{self, Mode};

#[derive(Debug, thiserror::Error)]
pub(crate) enum StoreError {
    #[error("cannot read {path}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cannot parse {path}, fix or remove it")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("{path} is at version {found}, written by a newer procly, this one reads version {expected}")]
    Version { path: PathBuf, found: u32, expected: u32 },
    #[error("cannot encode {path}, a path in it is not valid UTF-8")]
    Encode {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("cannot write {path}")]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// The body of a document, preceded by the version of its format.
#[derive(Debug, Serialize, Deserialize)]
struct Document<T> {
    version: u32,
    #[serde(flatten)]
    body: T,
}

/// A document of the state directory, with the version of its format.
pub(crate) trait Stored: DeserializeOwned {
    const VERSION: u32;
}

/// Whether the document `T` at `path` can be read, as the next request that needs it reads it. Nothing is kept or
/// written.
pub(crate) fn readable<T: Stored>(path: &Path) -> Result<(), StoreError> {
    load::<T>(path, T::VERSION).map(|_| ())
}

/// Reads the document at `path`, `None` when it does not exist yet. A document of an older version is read as it
/// is, a newer one is refused rather than misread.
pub(crate) fn load<T: DeserializeOwned>(path: &Path, version: u32) -> Result<Option<T>, StoreError> {
    #[derive(Deserialize)]
    struct Version {
        version: u32,
    }
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(StoreError::Read {
                path: path.to_owned(),
                source,
            });
        }
    };
    let parse_error = |source| StoreError::Parse {
        path: path.to_owned(),
        source,
    };
    let Version { version: found } = serde_json::from_str(&text).map_err(parse_error)?;
    if found > version {
        return Err(StoreError::Version {
            path: path.to_owned(),
            found,
            expected: version,
        });
    }
    let document: Document<T> = serde_json::from_str(&text).map_err(parse_error)?;
    Ok(Some(document.body))
}

/// Replaces the document at `path` in one step, readable by the user only, as the rest of the state directory.
pub(crate) fn save<T: Serialize>(path: &Path, version: u32, body: &T) -> Result<(), StoreError> {
    let mut text = serde_json::to_string_pretty(&Document { version, body }).map_err(|source| StoreError::Encode {
        path: path.to_owned(),
        source,
    })?;
    text.push('\n');
    atomic_file::replace(path, text.as_bytes(), Mode::Private).map_err(|source| StoreError::Write {
        path: path.to_owned(),
        source,
    })
}
