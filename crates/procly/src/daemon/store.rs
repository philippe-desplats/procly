//! The persistent state of the Daemon: one versioned JSON document per concern in the state directory (ADR 0012).
//!
//! The Daemon is the only writer, it holds the lock of the state directory, so a document needs no locking of its
//! own. Each one is replaced atomically, a crash leaves either the previous content or the new one.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

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

/// Replaces the document at `path`: written to a temporary file next to it, flushed to disk, then renamed over it.
/// The file is readable by the user only, as the rest of the state directory.
pub(crate) fn save<T: Serialize>(path: &Path, version: u32, body: &T) -> Result<(), StoreError> {
    let mut text = serde_json::to_string_pretty(&Document { version, body }).map_err(|source| StoreError::Encode {
        path: path.to_owned(),
        source,
    })?;
    text.push('\n');
    let temporary = path.with_extension("json.tmp");
    let write_error = |path: &Path| {
        let path = path.to_owned();
        move |source| StoreError::Write { path, source }
    };
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(write_error(&temporary))?;
    file.write_all(text.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(write_error(&temporary))?;
    fs::rename(&temporary, path).map_err(write_error(path))?;
    // The rename itself is durable only once the directory holding it is.
    if let Some(dir) = path.parent() {
        File::open(dir)
            .and_then(|dir| dir.sync_all())
            .map_err(write_error(dir))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Body {
        items: Vec<u16>,
    }

    #[test]
    fn a_document_round_trips_with_its_version_and_is_private() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("items.json");
        assert_eq!(load::<Body>(&path, 1).unwrap(), None);

        save(
            &path,
            1,
            &Body {
                items: vec![24000, 24001],
            },
        )
        .unwrap();

        assert_eq!(
            load::<Body>(&path, 1).unwrap(),
            Some(Body {
                items: vec![24000, 24001]
            })
        );
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("{\n  \"version\": 1,\n"), "{text}");
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert!(!path.with_extension("json.tmp").exists());
    }

    #[test]
    fn a_newer_or_malformed_document_is_refused_naming_its_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("items.json");
        fs::write(&path, "{\"version\": 2, \"items\": []}").unwrap();
        assert!(matches!(
            load::<Body>(&path, 1),
            Err(StoreError::Version { found: 2, .. })
        ));

        fs::write(&path, "{\"version\": 1, \"items\": [").unwrap();
        let error = load::<Body>(&path, 1).unwrap_err();
        assert!(error.to_string().contains("items.json"), "{error}");
    }
}
