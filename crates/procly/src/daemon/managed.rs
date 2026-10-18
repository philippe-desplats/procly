//! Managed blocks: text Procly writes into a file it does not own, delimited by markers and recorded with the
//! fingerprint of its content (ADR 0008).
//!
//! The record tells a block Procly wrote and nobody touched from one the user edited. Procly rewrites the first and
//! never the second.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use super::store::{self, StoreError};

/// The version of the format of the document of fingerprints, bumped when a field changes meaning or disappears.
const DOCUMENT_VERSION: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub(crate) enum ManagedError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("cannot read {path}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cannot write {path}")]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// What a request to write a block did to the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// The block was added or updated.
    Written,
    /// The file already held exactly this block.
    Unchanged,
    /// The block holds something Procly did not write, so it was left as it is.
    Edited,
}

/// The fingerprint of the content of every block Procly wrote, by file and block name, kept in a document of the
/// state directory so that it survives restarts of the Daemon.
#[derive(Debug)]
pub(crate) struct ManagedBlocks {
    path: PathBuf,
    /// Read from the document on the first request, as the Leases are. `None` until then.
    table: Mutex<Option<Table>>,
}

type Table = BTreeMap<(PathBuf, String), String>;

/// One fingerprint as the document stores it.
#[derive(Debug, Serialize, Deserialize)]
struct Record {
    file: PathBuf,
    block: String,
    fingerprint: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct ManagedDocument {
    blocks: Vec<Record>,
}

impl ManagedBlocks {
    /// The fingerprints stored in the document at `path`.
    pub(crate) fn new(path: PathBuf) -> Self {
        Self {
            path,
            table: Mutex::new(None),
        }
    }

    /// Makes `file` hold the block `block` with `content`, one line per element, creating the file and its directory
    /// when they are missing. The rest of the file is kept byte for byte. A block the user edited is left alone.
    pub(crate) fn write(&self, file: &Path, block: &str, content: &[impl AsRef<str>]) -> Result<Outcome, ManagedError> {
        let mut table = self
            .table
            .lock()
            .expect("the managed block table mutex is never poisoned, no code panics while holding it");
        let table = match &mut *table {
            Some(table) => table,
            None => table.insert(self.read()?),
        };
        let content: String = content.iter().flat_map(|line| [line.as_ref(), "\n"]).collect();
        let text = read_text(file)?;
        let key = (file.to_owned(), block.to_owned());
        let (change, new_text) = plan(&text, block, &content, table.get(&key).map(String::as_str));
        if let Some(new_text) = new_text {
            write_text(file, &new_text)?;
        }
        if change != Outcome::Edited {
            let fingerprint = fingerprint(&content);
            if table.get(&key) != Some(&fingerprint) {
                let mut updated = table.clone();
                updated.insert(key, fingerprint);
                self.write_table(&updated)?;
                *table = updated;
            }
        }
        Ok(change)
    }

    /// Whether `file` holds the block `block`, whatever its content.
    pub(crate) fn holds(file: &Path, block: &str) -> Result<bool, ManagedError> {
        Ok(!matches!(locate(&read_text(file)?, block), Located::Absent))
    }

    fn read(&self) -> Result<Table, StoreError> {
        let document = store::load::<ManagedDocument>(&self.path, DOCUMENT_VERSION)?;
        Ok(document
            .map(|document| {
                document
                    .blocks
                    .into_iter()
                    .map(|record| ((record.file, record.block), record.fingerprint))
                    .collect()
            })
            .unwrap_or_default())
    }

    fn write_table(&self, table: &Table) -> Result<(), StoreError> {
        let blocks = table
            .iter()
            .map(|((file, block), fingerprint)| Record {
                file: file.clone(),
                block: block.clone(),
                fingerprint: fingerprint.clone(),
            })
            .collect();
        store::save(&self.path, DOCUMENT_VERSION, &ManagedDocument { blocks })
    }
}

/// The text of `file`, empty when it does not exist.
fn read_text(file: &Path) -> Result<String, ManagedError> {
    match fs::read_to_string(file) {
        Ok(text) => Ok(text),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        Err(source) => Err(ManagedError::Read {
            path: file.to_owned(),
            source,
        }),
    }
}

/// Replaces `file` in one step, keeping its permissions, so that a crash never leaves a half-written `.gitignore`.
fn write_text(file: &Path, text: &str) -> Result<(), ManagedError> {
    let write_error = |source| ManagedError::Write {
        path: file.to_owned(),
        source,
    };
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir).map_err(write_error)?;
    }
    let mut partial = file.as_os_str().to_owned();
    partial.push(".procly-partial");
    let partial = PathBuf::from(partial);
    fs::write(&partial, text).map_err(write_error)?;
    if let Ok(metadata) = fs::metadata(file) {
        fs::set_permissions(&partial, metadata.permissions()).map_err(write_error)?;
    }
    fs::rename(&partial, file).map_err(write_error)
}

/// What to do with `text` to make it hold the block `block` with `content`, and the text to write, if any.
/// `recorded` is the fingerprint stored for the block when Procly last wrote it.
fn plan(text: &str, block: &str, content: &str, recorded: Option<&str>) -> (Outcome, Option<String>) {
    match locate(text, block) {
        Located::Absent => {
            let mut appended = text.to_owned();
            if !appended.is_empty() && !appended.ends_with('\n') {
                appended.push('\n');
            }
            appended.push_str(&render(block, content));
            (Outcome::Written, Some(appended))
        }
        Located::Found { content: found, .. } if found == content => (Outcome::Unchanged, None),
        Located::Found { range, content: found } if recorded == Some(fingerprint(&found).as_str()) => {
            let mut replaced = text.to_owned();
            replaced.replace_range(range, &render(block, content));
            (Outcome::Written, Some(replaced))
        }
        Located::Broken | Located::Found { .. } => (Outcome::Edited, None),
    }
}

/// Where the block `block` sits in a text.
#[derive(Debug, PartialEq, Eq)]
enum Located {
    Absent,
    /// The block opens and closes: the span of its lines, markers included, and what sits between them.
    Found {
        range: Range<usize>,
        content: String,
    },
    /// The block opens and never closes, so its end is unknown.
    Broken,
}

fn open_marker(block: &str) -> String {
    format!("# >>> procly managed block: {block} >>>")
}

fn close_marker(block: &str) -> String {
    format!("# <<< procly managed block: {block} <<<")
}

/// The block as it is written: its markers around `content`, which ends with a line break.
fn render(block: &str, content: &str) -> String {
    format!("{}\n{content}{}\n", open_marker(block), close_marker(block))
}

fn locate(text: &str, block: &str) -> Located {
    let (open, close) = (open_marker(block), close_marker(block));
    let mut offset = 0;
    let mut start = None;
    for line in text.split_inclusive('\n') {
        let end = offset + line.len();
        let marker = line.trim_end_matches(['\n', '\r']);
        match start {
            None if marker == open => start = Some((offset, end)),
            Some((block_start, content_start)) if marker == close => {
                return Located::Found {
                    range: block_start..end,
                    content: text[content_start..offset].to_owned(),
                };
            }
            _ => {}
        }
        offset = end;
    }
    if start.is_some() {
        Located::Broken
    } else {
        Located::Absent
    }
}

/// FNV-1a over the bytes of `content`, 64 bits, in hexadecimal. It is written out instead of taken from the standard
/// library, whose hashers change between releases, because a fingerprint stored today is compared by a later Procly.
/// It tells an edit from no edit and is no defense against someone crafting a collision.
fn fingerprint(content: &str) -> String {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let hash = content
        .bytes()
        .fold(OFFSET_BASIS, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(PRIME));
    format!("fnv1a64:{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fingerprint_is_fnv_1a_64_of_the_content() {
        // The published test vectors of the algorithm.
        assert_eq!(fingerprint(""), "fnv1a64:cbf29ce484222325");
        assert_eq!(fingerprint("a"), "fnv1a64:af63dc4c8601ec8c");
        assert_eq!(fingerprint("foobar"), "fnv1a64:85944171f73967e8");
    }

    fn block(content: &str) -> String {
        render("exclusions", content)
    }

    #[test]
    fn a_missing_block_is_appended_after_the_rest_of_the_file() {
        let (change, text) = plan("node_modules\n", "exclusions", "/a\n", None);
        assert_eq!(change, Outcome::Written);
        assert_eq!(text.unwrap(), format!("node_modules\n{}", block("/a\n")));
    }

    #[test]
    fn a_file_without_a_final_line_break_gets_one_and_nothing_else_changes() {
        let (_, text) = plan("a\nb", "exclusions", "/a\n", None);
        assert_eq!(text.unwrap(), format!("a\nb\n{}", block("/a\n")));
        let (_, text) = plan("", "exclusions", "/a\n", None);
        assert_eq!(text.unwrap(), block("/a\n"));
    }

    #[test]
    fn a_block_with_the_same_content_is_left_alone() {
        let text = format!("above\n{}below", block("/a\n"));
        assert_eq!(plan(&text, "exclusions", "/a\n", None), (Outcome::Unchanged, None));
    }

    #[test]
    fn an_intact_block_is_updated_in_place_between_the_lines_around_it() {
        let text = format!("above\n{}below", block("/a\n"));
        let recorded = fingerprint("/a\n");
        let (change, updated) = plan(&text, "exclusions", "/a\n/b\n", Some(&recorded));
        assert_eq!(change, Outcome::Written);
        assert_eq!(updated.unwrap(), format!("above\n{}below", block("/a\n/b\n")));
    }

    #[test]
    fn an_edited_block_is_never_overwritten() {
        let text = format!("above\n{}below", block("/a\n/mine\n"));
        let recorded = fingerprint("/a\n");
        assert_eq!(
            plan(&text, "exclusions", "/a\n/b\n", Some(&recorded)),
            (Outcome::Edited, None)
        );
        // A block whose fingerprint was never recorded is no block of ours to rewrite either.
        assert_eq!(plan(&text, "exclusions", "/a\n/b\n", None), (Outcome::Edited, None));
    }

    #[test]
    fn a_block_that_never_closes_is_left_alone() {
        let text = format!("{}\n/a\n", open_marker("exclusions"));
        assert_eq!(plan(&text, "exclusions", "/a\n", None), (Outcome::Edited, None));
    }

    #[test]
    fn blocks_of_other_names_do_not_count() {
        let text = render("rules", "x\n");
        assert_eq!(locate(&text, "exclusions"), Located::Absent);
        assert_eq!(plan(&text, "exclusions", "/a\n", None).0, Outcome::Written);
    }

    #[test]
    fn writing_records_the_fingerprint_and_a_second_write_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let blocks = ManagedBlocks::new(dir.path().join("managed-blocks.json"));
        let file = dir.path().join("info/exclude");

        assert_eq!(blocks.write(&file, "exclusions", &["/a"]).unwrap(), Outcome::Written);
        let first = fs::read(&file).unwrap();
        assert_eq!(blocks.write(&file, "exclusions", &["/a"]).unwrap(), Outcome::Unchanged);
        assert_eq!(fs::read(&file).unwrap(), first);

        // A new Daemon reads the record back, so it still tells an intact block from an edited one.
        let restarted = ManagedBlocks::new(dir.path().join("managed-blocks.json"));
        assert_eq!(
            restarted.write(&file, "exclusions", &["/a", "/b"]).unwrap(),
            Outcome::Written
        );
        let text = fs::read_to_string(&file).unwrap();
        assert!(text.contains("/a\n/b\n"), "{text}");
        let edited = text.replace("/b\n", "/mine\n");
        fs::write(&file, &edited).unwrap();
        assert_eq!(
            restarted.write(&file, "exclusions", &["/a", "/b"]).unwrap(),
            Outcome::Edited
        );
        assert_eq!(fs::read_to_string(&file).unwrap(), edited);
    }

    #[test]
    fn a_file_holds_a_block_whatever_its_content() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(".gitignore");
        assert!(!ManagedBlocks::holds(&file, "exclusions").unwrap());
        fs::write(&file, render("exclusions", "anything\n")).unwrap();
        assert!(ManagedBlocks::holds(&file, "exclusions").unwrap());
        assert!(!ManagedBlocks::holds(&file, "rules").unwrap());
    }
}
