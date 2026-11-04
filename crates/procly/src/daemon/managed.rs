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

use super::atomic_file::{self, Mode};
use super::fnv;
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
    #[error("cannot remove {path}")]
    Remove {
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

/// What a request to remove a block did to the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Removal {
    /// The block was taken out. `deleted` tells that the file held nothing else, so it was deleted too.
    Removed { deleted: bool },
    /// The file does not hold the block.
    Absent,
    /// The block holds something Procly did not write, so it was left as it is.
    Edited,
}

/// What a file holds for a block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Content {
    /// The file does not exist or holds no marker of the block.
    Absent,
    /// The block opens and never closes, so its end is unknown.
    Unclosed,
    /// The block opens and closes: what sits between its markers.
    Closed(String),
}

/// The fingerprint of the content of every block Procly wrote, by file and block name, kept in a document of the
/// state directory so that it survives restarts of the Daemon.
#[derive(Debug)]
pub(crate) struct ManagedBlocks {
    path: PathBuf,
    /// Read from the document on the first request, as the Leases are. `None` until then.
    table: Mutex<Option<Table>>,
}

type Table = BTreeMap<(PathBuf, String), Entry>;

/// What is known of a block: the fingerprint of its content, and how its file looked before Procly wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    fingerprint: String,
    /// `None` for a block recorded by a Procly that did not keep it, or whose file was edited around the block.
    origin: Option<Origin>,
}

/// How a file looked just before Procly first wrote a block into it, which is what eject needs to give it back byte
/// for byte.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Origin {
    /// Whether the file existed. One that did not is deleted again once the block is out.
    pub(crate) existed: bool,
    /// Whether the file lacked a final line break, so that Procly added one before its block.
    #[serde(default)]
    pub(crate) newline_added: bool,
    /// The text of a file Procly writes again as a whole, such as a JSON settings file, whose layout cannot be
    /// rebuilt from what it holds now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) original: Option<String>,
}

/// One block as the document stores it.
#[derive(Debug, Serialize, Deserialize)]
struct Record {
    file: PathBuf,
    block: String,
    fingerprint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    origin: Option<Origin>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct ManagedDocument {
    blocks: Vec<Record>,
}

impl store::Stored for ManagedDocument {
    const VERSION: u32 = DOCUMENT_VERSION;
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
        self.with_table(|table| {
            let content = joined(content);
            let existing = read_existing(file)?;
            let text = existing.clone().unwrap_or_default();
            let key = (file.to_owned(), block.to_owned());
            let syntax = Syntax::of(file);
            let appended = matches!(locate(syntax, &text, block), Located::Absent);
            let (change, new_text) = plan(
                syntax,
                &text,
                block,
                &content,
                table.get(&key).map(|entry| entry.fingerprint.as_str()),
            );
            if let Some(new_text) = new_text {
                write_text(file, &new_text)?;
            }
            if change != Outcome::Edited {
                // A block added to the file as it is now. One updated in place keeps how the file looked before it.
                let origin = appended.then(|| Origin {
                    existed: existing.is_some(),
                    // A block put first leaves the end of the file as it was.
                    newline_added: syntax != Syntax::Toml && !text.is_empty() && !text.ends_with('\n'),
                    original: None,
                });
                self.remember(table, key, fingerprint(&content), origin)?;
            }
            Ok(change)
        })
    }

    /// Takes the block `block` out of `file` when Procly wrote it and nobody edited it, and forgets its record. The
    /// rest of the file is kept byte for byte, and the file is given back as it was before the block: a line break
    /// Procly added goes, a file that did not exist is deleted once empty, and one that did is kept, even empty.
    /// `known` lists the contents that count as Procly's even without a fingerprint, such as the one a Procly whose
    /// state was lost would write. A block the user edited, or one that never closes, is left alone.
    pub(crate) fn remove(&self, file: &Path, block: &str, known: &[String]) -> Result<Removal, ManagedError> {
        self.with_table(|table| {
            let key = (file.to_owned(), block.to_owned());
            let text = read_text(file)?;
            let entry = table.get(&key);
            let origin = entry.and_then(|entry| entry.origin.as_ref());
            let removal = match plan_removal(
                Syntax::of(file),
                &text,
                block,
                known,
                entry.map(|entry| entry.fingerprint.as_str()),
                origin,
            ) {
                Planned::Edited => return Ok(Removal::Edited),
                Planned::Absent => Removal::Absent,
                Planned::Remove(rest) if rest.is_empty() && !origin.is_some_and(|origin| origin.existed) => {
                    fs::remove_file(file).map_err(|source| ManagedError::Remove {
                        path: file.to_owned(),
                        source,
                    })?;
                    Removal::Removed { deleted: true }
                }
                Planned::Remove(rest) => {
                    write_text(file, &rest)?;
                    Removal::Removed { deleted: false }
                }
            };
            self.forget(table, &key)?;
            Ok(removal)
        })
    }

    /// How the file of the block `block` looked before Procly wrote it, when Procly recorded that.
    pub(crate) fn origin(&self, file: &Path, block: &str) -> Result<Option<Origin>, ManagedError> {
        self.with_table(|table| {
            Ok(table
                .get(&(file.to_owned(), block.to_owned()))
                .and_then(|entry| entry.origin.clone()))
        })
    }

    /// Forgets the fingerprint of the block `block` of `file`, for a file that cannot carry markers and whose caller
    /// took the block out by its own means.
    pub(crate) fn forget_block(&self, file: &Path, block: &str) -> Result<(), ManagedError> {
        self.with_table(|table| self.forget(table, &(file.to_owned(), block.to_owned())))
    }

    /// What `file` holds for the block `block`, read without writing anything.
    pub(crate) fn content(file: &Path, block: &str) -> Result<Content, ManagedError> {
        Ok(match locate(Syntax::of(file), &read_text(file)?, block) {
            Located::Absent => Content::Absent,
            Located::Broken => Content::Unclosed,
            Located::Found { content, .. } => Content::Closed(content),
        })
    }

    /// The text of `file` without the block `block`, markers included, which is all of it when the file lacks the block
    /// or when the block never closes. `None` when the file does not exist.
    pub(crate) fn outside(file: &Path, block: &str) -> Result<Option<String>, ManagedError> {
        Ok(
            read_existing(file)?.map(|text| match locate(Syntax::of(file), &text, block) {
                Located::Found { range, .. } => {
                    let mut outside = text;
                    outside.replace_range(range, "");
                    outside
                }
                Located::Absent | Located::Broken => text,
            }),
        )
    }

    /// Whether `file` holds the block `block`, whatever its content.
    pub(crate) fn holds(file: &Path, block: &str) -> Result<bool, ManagedError> {
        Ok(!matches!(
            locate(Syntax::of(file), &read_text(file)?, block),
            Located::Absent
        ))
    }

    /// The files Procly recorded a block named `block` in, whether they still hold it or not.
    pub(crate) fn files_holding(&self, block: &str) -> Result<Vec<PathBuf>, ManagedError> {
        self.with_table(|table| {
            Ok(table
                .keys()
                .filter(|(_, name)| name == block)
                .map(|(file, _)| file.clone())
                .collect())
        })
    }

    /// Records that Procly wrote `content` as the block `block` of `file`, for a file that cannot carry markers, such
    /// as JSON. The caller finds the content again by its own means and asks [`ManagedBlocks::is_recorded`]. `origin`
    /// is how the file looked before the block, or `None` to keep what was recorded already.
    pub(crate) fn record(
        &self,
        file: &Path,
        block: &str,
        content: &str,
        origin: Option<Origin>,
    ) -> Result<(), ManagedError> {
        self.with_table(|table| self.remember(table, (file.to_owned(), block.to_owned()), fingerprint(content), origin))
    }

    /// Whether the block `block` of `file` was last recorded as exactly `content`, which tells a block Procly wrote
    /// from one the user edited.
    pub(crate) fn is_recorded(&self, file: &Path, block: &str, content: &str) -> Result<bool, ManagedError> {
        self.with_table(|table| {
            Ok(table
                .get(&(file.to_owned(), block.to_owned()))
                .map(|entry| entry.fingerprint.as_str())
                == Some(fingerprint(content).as_str()))
        })
    }

    /// Whether the block `block` of `file` was recorded as another content than `content`. A block that was never
    /// recorded has not been edited as far as the record knows, so this is false for it.
    pub(crate) fn departs_from_record(&self, file: &Path, block: &str, content: &str) -> Result<bool, ManagedError> {
        self.with_table(|table| {
            Ok(table
                .get(&(file.to_owned(), block.to_owned()))
                .is_some_and(|recorded| recorded.fingerprint != fingerprint(content)))
        })
    }

    /// Runs `action` on the table of fingerprints, read from the document the first time.
    fn with_table<T>(&self, action: impl FnOnce(&mut Table) -> Result<T, ManagedError>) -> Result<T, ManagedError> {
        let mut table = self
            .table
            .lock()
            .expect("the managed block table mutex is never poisoned, no code panics while holding it");
        let table = match &mut *table {
            Some(table) => table,
            None => table.insert(self.read()?),
        };
        action(table)
    }

    /// Stores `fingerprint` for `key`, in the document first so that a failed write leaves the table as it was. The
    /// `origin` already stored stays when none is given.
    fn remember(
        &self,
        table: &mut Table,
        key: (PathBuf, String),
        fingerprint: String,
        origin: Option<Origin>,
    ) -> Result<(), ManagedError> {
        let origin = origin.or_else(|| table.get(&key).and_then(|entry| entry.origin.clone()));
        let entry = Entry { fingerprint, origin };
        if table.get(&key) != Some(&entry) {
            let mut updated = table.clone();
            updated.insert(key, entry);
            self.write_table(&updated)?;
            *table = updated;
        }
        Ok(())
    }

    /// Drops the fingerprint for `key`, in the document first so that a failed write leaves the table as it was.
    fn forget(&self, table: &mut Table, key: &(PathBuf, String)) -> Result<(), ManagedError> {
        if table.contains_key(key) {
            let mut updated = table.clone();
            updated.remove(key);
            self.write_table(&updated)?;
            *table = updated;
        }
        Ok(())
    }

    fn read(&self) -> Result<Table, StoreError> {
        let document = store::load::<ManagedDocument>(&self.path, DOCUMENT_VERSION)?;
        Ok(document
            .map(|document| {
                document
                    .blocks
                    .into_iter()
                    .map(|record| {
                        (
                            (record.file, record.block),
                            Entry {
                                fingerprint: record.fingerprint,
                                origin: record.origin,
                            },
                        )
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    fn write_table(&self, table: &Table) -> Result<(), StoreError> {
        let blocks = table
            .iter()
            .map(|((file, block), entry)| Record {
                file: file.clone(),
                block: block.clone(),
                fingerprint: entry.fingerprint.clone(),
                origin: entry.origin.clone(),
            })
            .collect();
        store::save(&self.path, DOCUMENT_VERSION, &ManagedDocument { blocks })
    }
}

/// The text of `file`, empty when it does not exist.
fn read_text(file: &Path) -> Result<String, ManagedError> {
    Ok(read_existing(file)?.unwrap_or_default())
}

/// The text of `file`, `None` when it does not exist.
pub(super) fn read_existing(file: &Path) -> Result<Option<String>, ManagedError> {
    match fs::read_to_string(file) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(ManagedError::Read {
            path: file.to_owned(),
            source,
        }),
    }
}

/// Replaces `file` in one step, keeping its permissions, so that a crash never leaves a half-written `.gitignore`.
pub(super) fn write_text(file: &Path, text: &str) -> Result<(), ManagedError> {
    let write_error = |source| ManagedError::Write {
        path: file.to_owned(),
        source,
    };
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir).map_err(write_error)?;
    }
    atomic_file::replace(file, text.as_bytes(), Mode::Kept).map_err(write_error)
}

/// What to do with `text` to make it hold the block `block` with `content`, and the text to write, if any.
/// `recorded` is the fingerprint stored for the block when Procly last wrote it.
fn plan(syntax: Syntax, text: &str, block: &str, content: &str, recorded: Option<&str>) -> (Outcome, Option<String>) {
    match locate(syntax, text, block) {
        Located::Absent if syntax == Syntax::Toml => (
            Outcome::Written,
            Some(format!("{}{text}", render(syntax, block, content))),
        ),
        Located::Absent => {
            let mut appended = text.to_owned();
            if !appended.is_empty() && !appended.ends_with('\n') {
                appended.push('\n');
            }
            appended.push_str(&render(syntax, block, content));
            (Outcome::Written, Some(appended))
        }
        Located::Found { content: found, .. } if found == content => (Outcome::Unchanged, None),
        Located::Found { range, content: found } if recorded == Some(fingerprint(&found).as_str()) => {
            let mut replaced = text.to_owned();
            replaced.replace_range(range, &render(syntax, block, content));
            (Outcome::Written, Some(replaced))
        }
        Located::Broken | Located::Found { .. } => (Outcome::Edited, None),
    }
}

/// What to do with `text` to take the block `block` out of it.
#[derive(Debug, PartialEq, Eq)]
enum Planned {
    Absent,
    Edited,
    /// The text without the block.
    Remove(String),
}

/// `recorded` is the fingerprint stored for the block when Procly last wrote it, `known` the contents that are
/// Procly's whatever the record says, and `origin` how the file looked before the block.
fn plan_removal(
    syntax: Syntax,
    text: &str,
    block: &str,
    known: &[String],
    recorded: Option<&str>,
    origin: Option<&Origin>,
) -> Planned {
    match locate(syntax, text, block) {
        Located::Absent => Planned::Absent,
        Located::Found { range, content }
            if recorded == Some(fingerprint(&content).as_str()) || known.contains(&content) =>
        {
            let at_end = range.end == text.len();
            let mut rest = text.to_owned();
            rest.replace_range(range, "");
            // The line break Procly put before the block goes with it, unless something was written after the block.
            if at_end && origin.is_some_and(|origin| origin.newline_added) && rest.ends_with('\n') {
                rest.pop();
            }
            Planned::Remove(rest)
        }
        Located::Broken | Located::Found { .. } => Planned::Edited,
    }
}

/// The lines of a block as its content: each one ends with a line break.
pub(super) fn joined(lines: &[impl AsRef<str>]) -> String {
    lines.iter().flat_map(|line| [line.as_ref(), "\n"]).collect()
}

/// The lines that open and close the block `block` in `file`, which depend on the comments the file can hold.
pub(super) fn markers(file: &Path, block: &str) -> (String, String) {
    let syntax = Syntax::of(file);
    (open_marker(syntax, block), close_marker(syntax, block))
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

/// How a file marks the lines that belong to Procly, which depends on the comments the file can hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Syntax {
    /// `# …` lines, for the git exclude files.
    Hash,
    /// `<!-- … -->` lines, for Markdown, where a `#` line would render as a heading.
    Html,
    /// `# …` lines, for TOML. A key written after a table header belongs to that table, so a block of root keys goes
    /// first in the file instead of last.
    Toml,
}

impl Syntax {
    /// The syntax a file takes, from its extension.
    fn of(file: &Path) -> Self {
        match file.extension().and_then(|extension| extension.to_str()) {
            Some("md" | "mdc" | "markdown") => Self::Html,
            Some("toml") => Self::Toml,
            _ => Self::Hash,
        }
    }

    fn comment(self, text: &str) -> String {
        match self {
            Self::Hash | Self::Toml => format!("# {text}"),
            Self::Html => format!("<!-- {text} -->"),
        }
    }
}

fn open_marker(syntax: Syntax, block: &str) -> String {
    syntax.comment(&format!(">>> procly managed block: {block} >>>"))
}

fn close_marker(syntax: Syntax, block: &str) -> String {
    syntax.comment(&format!("<<< procly managed block: {block} <<<"))
}

/// The block as it is written: its markers around `content`, which ends with a line break.
fn render(syntax: Syntax, block: &str, content: &str) -> String {
    format!(
        "{}\n{content}{}\n",
        open_marker(syntax, block),
        close_marker(syntax, block)
    )
}

fn locate(syntax: Syntax, text: &str, block: &str) -> Located {
    let (open, close) = (open_marker(syntax, block), close_marker(syntax, block));
    let mut offset = 0;
    let mut start = None;
    for line in text.split_inclusive('\n') {
        let end = offset + line.len();
        let marker = line.trim_end();
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

/// FNV-1a 64 of `content`, in hexadecimal, since a fingerprint stored today is compared by a later Procly. It tells an
/// edit from no edit.
fn fingerprint(content: &str) -> String {
    format!("fnv1a64:{:016x}", fnv::fnv1a64(content.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    #[test]
    fn the_fingerprint_is_fnv_1a_64_of_the_content() {
        // The published test vectors of the algorithm.
        assert_eq!(fingerprint(""), "fnv1a64:cbf29ce484222325");
        assert_eq!(fingerprint("a"), "fnv1a64:af63dc4c8601ec8c");
        assert_eq!(fingerprint("foobar"), "fnv1a64:85944171f73967e8");
    }

    fn block(content: &str) -> String {
        render(Syntax::Hash, "exclusions", content)
    }

    #[test]
    fn a_missing_block_is_appended_after_the_rest_of_the_file() {
        let (change, text) = plan(Syntax::Hash, "node_modules\n", "exclusions", "/a\n", None);
        assert_eq!(change, Outcome::Written);
        assert_eq!(text.unwrap(), format!("node_modules\n{}", block("/a\n")));
    }

    #[test]
    fn a_block_of_a_toml_file_goes_first_so_that_its_keys_stay_at_the_root() {
        let (change, text) = plan(Syntax::Toml, "a = 1\n[table]\nb = 2", "worktrunk", "c = 3\n", None);
        assert_eq!(change, Outcome::Written);
        assert_eq!(
            text.unwrap(),
            format!("{}a = 1\n[table]\nb = 2", render(Syntax::Toml, "worktrunk", "c = 3\n"))
        );
        assert_eq!(Syntax::of(Path::new("/p/.config/wt.toml")), Syntax::Toml);
    }

    #[test]
    fn a_toml_block_is_removed_and_the_file_comes_back_byte_for_byte() {
        let original = "a = 1\n[table]\nb = 2";
        let (_, written) = plan(Syntax::Toml, original, "worktrunk", "c = 3\n", None);
        let recorded = fingerprint("c = 3\n");
        let origin = Origin {
            existed: true,
            newline_added: false,
            original: None,
        };

        let planned = plan_removal(
            Syntax::Toml,
            &written.unwrap(),
            "worktrunk",
            &[],
            Some(&recorded),
            Some(&origin),
        );

        assert_eq!(planned, Planned::Remove(original.to_owned()));
    }

    #[test]
    fn the_text_outside_a_block_is_the_file_without_its_lines() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("wt.toml");
        assert_eq!(ManagedBlocks::outside(&file, "worktrunk").unwrap(), None);
        fs::write(
            &file,
            format!("a = 1\n{}b = 2\n", render(Syntax::Toml, "worktrunk", "c = 3\n")),
        )
        .unwrap();
        assert_eq!(
            ManagedBlocks::outside(&file, "worktrunk").unwrap().as_deref(),
            Some("a = 1\nb = 2\n")
        );
        assert_eq!(
            ManagedBlocks::outside(&file, "other").unwrap().as_deref().map(str::len),
            Some(fs::read_to_string(&file).unwrap().len())
        );
    }

    #[test]
    fn a_file_without_a_final_line_break_gets_one_and_nothing_else_changes() {
        let (_, text) = plan(Syntax::Hash, "a\nb", "exclusions", "/a\n", None);
        assert_eq!(text.unwrap(), format!("a\nb\n{}", block("/a\n")));
        let (_, text) = plan(Syntax::Hash, "", "exclusions", "/a\n", None);
        assert_eq!(text.unwrap(), block("/a\n"));
    }

    #[test]
    fn a_block_with_the_same_content_is_left_alone() {
        let text = format!("above\n{}below", block("/a\n"));
        assert_eq!(
            plan(Syntax::Hash, &text, "exclusions", "/a\n", None),
            (Outcome::Unchanged, None)
        );
    }

    #[test]
    fn an_intact_block_is_updated_in_place_between_the_lines_around_it() {
        let text = format!("above\n{}below", block("/a\n"));
        let recorded = fingerprint("/a\n");
        let (change, updated) = plan(Syntax::Hash, &text, "exclusions", "/a\n/b\n", Some(&recorded));
        assert_eq!(change, Outcome::Written);
        assert_eq!(updated.unwrap(), format!("above\n{}below", block("/a\n/b\n")));
    }

    #[test]
    fn an_edited_block_is_never_overwritten() {
        let text = format!("above\n{}below", block("/a\n/mine\n"));
        let recorded = fingerprint("/a\n");
        assert_eq!(
            plan(Syntax::Hash, &text, "exclusions", "/a\n/b\n", Some(&recorded)),
            (Outcome::Edited, None)
        );
        // A block whose fingerprint was never recorded is no block of ours to rewrite either.
        assert_eq!(
            plan(Syntax::Hash, &text, "exclusions", "/a\n/b\n", None),
            (Outcome::Edited, None)
        );
    }

    #[test]
    fn a_block_that_never_closes_is_left_alone() {
        let text = format!("{}\n/a\n", open_marker(Syntax::Hash, "exclusions"));
        assert_eq!(
            plan(Syntax::Hash, &text, "exclusions", "/a\n", None),
            (Outcome::Edited, None)
        );
    }

    #[test]
    fn blocks_of_other_names_do_not_count() {
        let text = render(Syntax::Hash, "rules", "x\n");
        assert_eq!(locate(Syntax::Hash, &text, "exclusions"), Located::Absent);
        assert_eq!(
            plan(Syntax::Hash, &text, "exclusions", "/a\n", None).0,
            Outcome::Written
        );
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
        fs::write(&file, render(Syntax::Hash, "exclusions", "anything\n")).unwrap();
        assert!(ManagedBlocks::holds(&file, "exclusions").unwrap());
        assert!(!ManagedBlocks::holds(&file, "rules").unwrap());
    }

    #[test]
    fn a_markdown_file_is_marked_with_html_comments_and_the_other_files_with_hash_lines() {
        assert_eq!(Syntax::of(Path::new("AGENTS.md")), Syntax::Html);
        assert_eq!(Syntax::of(Path::new("rules.mdc")), Syntax::Html);
        assert_eq!(Syntax::of(Path::new(".gitignore")), Syntax::Hash);
        assert_eq!(Syntax::of(Path::new("info/exclude")), Syntax::Hash);
        assert_eq!(
            render(Syntax::Html, "rules", "text\n"),
            "<!-- >>> procly managed block: rules >>> -->\ntext\n<!-- <<< procly managed block: rules <<< -->\n"
        );
    }

    #[test]
    fn a_marker_line_an_editor_padded_with_spaces_still_marks_the_block() {
        let text = render(Syntax::Html, "rules", "x\n").replace("-->\n", "-->  \n");
        assert!(matches!(locate(Syntax::Html, &text, "rules"), Located::Found { .. }));
    }

    #[test]
    fn a_markdown_block_is_found_by_its_html_markers_and_not_by_hash_ones() {
        let html = render(Syntax::Html, "rules", "x\n");
        assert!(matches!(locate(Syntax::Html, &html, "rules"), Located::Found { .. }));
        assert_eq!(locate(Syntax::Hash, &html, "rules"), Located::Absent);
    }

    #[test]
    fn a_recorded_block_is_told_from_one_that_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let blocks = ManagedBlocks::new(dir.path().join("managed-blocks.json"));
        let file = dir.path().join("settings.json");

        assert!(!blocks.is_recorded(&file, "guard", "{}").unwrap());
        blocks.record(&file, "guard", "{}", None).unwrap();
        assert!(blocks.is_recorded(&file, "guard", "{}").unwrap());
        assert!(!blocks.is_recorded(&file, "guard", "{ }").unwrap());

        // A new Daemon reads the record back, and lists the files holding a block of that name.
        let restarted = ManagedBlocks::new(dir.path().join("managed-blocks.json"));
        assert!(restarted.is_recorded(&file, "guard", "{}").unwrap());
        assert_eq!(restarted.files_holding("guard").unwrap(), vec![file]);
        assert_eq!(restarted.files_holding("rules").unwrap(), Vec::<PathBuf>::new());
    }

    #[test]
    fn an_intact_block_is_taken_out_and_the_text_around_it_is_kept() {
        let text = format!("above\n{}below", block("/a\n"));
        let recorded = fingerprint("/a\n");
        assert_eq!(
            plan_removal(Syntax::Hash, &text, "exclusions", &[], Some(&recorded), None),
            Planned::Remove("above\nbelow".to_owned())
        );
    }

    #[test]
    fn a_block_with_a_known_content_is_taken_out_without_a_fingerprint() {
        let text = block("/a\n");
        let known = ["/b\n".to_owned(), "/a\n".to_owned()];
        assert_eq!(
            plan_removal(Syntax::Hash, &text, "exclusions", &known, None, None),
            Planned::Remove(String::new())
        );
    }

    #[test]
    fn an_edited_block_or_one_that_never_closes_is_not_taken_out() {
        let text = format!("above\n{}below", block("/a\n/mine\n"));
        let recorded = fingerprint("/a\n");
        assert_eq!(
            plan_removal(
                Syntax::Hash,
                &text,
                "exclusions",
                &["/a\n".to_owned()],
                Some(&recorded),
                None
            ),
            Planned::Edited
        );
        let open = format!("{}\n/a\n", open_marker(Syntax::Hash, "exclusions"));
        assert_eq!(
            plan_removal(Syntax::Hash, &open, "exclusions", &["/a\n".to_owned()], None, None),
            Planned::Edited
        );
        assert_eq!(
            plan_removal(Syntax::Hash, "other\n", "exclusions", &[], None, None),
            Planned::Absent
        );
    }

    #[test]
    fn removing_a_block_forgets_it_and_deletes_a_file_left_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("managed-blocks.json");
        let blocks = ManagedBlocks::new(path.clone());
        let file = dir.path().join("AGENTS.md");
        fs::write(&file, "mine\n").unwrap();
        blocks.write(&file, "rules", &["x"]).unwrap();
        let created = dir.path().join("info/exclude");
        blocks.write(&created, "exclusions", &["/a"]).unwrap();

        assert_eq!(
            blocks.remove(&file, "rules", &[]).unwrap(),
            Removal::Removed { deleted: false }
        );
        assert_eq!(fs::read_to_string(&file).unwrap(), "mine\n");
        assert_eq!(
            blocks.remove(&created, "exclusions", &[]).unwrap(),
            Removal::Removed { deleted: true }
        );
        assert!(!created.exists());

        // A new Daemon reads the record back: nothing is left in it.
        let restarted = ManagedBlocks::new(path);
        assert_eq!(restarted.files_holding("rules").unwrap(), Vec::<PathBuf>::new());
        assert_eq!(restarted.files_holding("exclusions").unwrap(), Vec::<PathBuf>::new());
        assert_eq!(restarted.remove(&file, "rules", &[]).unwrap(), Removal::Absent);
    }

    #[test]
    fn a_file_comes_back_without_the_line_break_procly_added_and_with_the_one_it_had() {
        let dir = tempfile::tempdir().unwrap();
        let blocks = ManagedBlocks::new(dir.path().join("managed-blocks.json"));
        let without = dir.path().join("without");
        let with = dir.path().join("with");
        fs::write(&without, "a\nb").unwrap();
        fs::write(&with, "a\nb\n").unwrap();
        blocks.write(&without, "exclusions", &["/a"]).unwrap();
        blocks.write(&with, "exclusions", &["/a"]).unwrap();
        // Updating the block in place changes nothing about how the file looked before it.
        blocks.write(&without, "exclusions", &["/a", "/b"]).unwrap();

        blocks.remove(&without, "exclusions", &[]).unwrap();
        blocks.remove(&with, "exclusions", &[]).unwrap();

        assert_eq!(fs::read(&without).unwrap(), b"a\nb");
        assert_eq!(fs::read(&with).unwrap(), b"a\nb\n");
    }

    #[test]
    fn text_written_after_the_block_keeps_the_line_break_that_joins_it_to_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let blocks = ManagedBlocks::new(dir.path().join("managed-blocks.json"));
        let file = dir.path().join("file");
        fs::write(&file, "a\nb").unwrap();
        blocks.write(&file, "exclusions", &["/a"]).unwrap();
        fs::OpenOptions::new()
            .append(true)
            .open(&file)
            .unwrap()
            .write_all(b"mine\n")
            .unwrap();

        blocks.remove(&file, "exclusions", &[]).unwrap();

        assert_eq!(fs::read(&file).unwrap(), b"a\nb\nmine\n");
    }

    #[test]
    fn a_file_that_existed_empty_stays_and_one_that_did_not_exist_goes() {
        let dir = tempfile::tempdir().unwrap();
        let blocks = ManagedBlocks::new(dir.path().join("managed-blocks.json"));
        let empty = dir.path().join("empty");
        let missing = dir.path().join("missing");
        fs::write(&empty, "").unwrap();
        blocks.write(&empty, "rules", &["x"]).unwrap();
        blocks.write(&missing, "rules", &["x"]).unwrap();

        assert_eq!(
            blocks.remove(&empty, "rules", &[]).unwrap(),
            Removal::Removed { deleted: false }
        );
        assert_eq!(
            blocks.remove(&missing, "rules", &[]).unwrap(),
            Removal::Removed { deleted: true }
        );

        assert_eq!(fs::read(&empty).unwrap(), b"");
        assert!(!missing.exists());
    }

    #[test]
    fn the_origin_of_a_file_survives_a_restart_of_the_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("managed-blocks.json");
        let file = dir.path().join("file");
        fs::write(&file, "a").unwrap();
        ManagedBlocks::new(path.clone()).write(&file, "rules", &["x"]).unwrap();

        let restarted = ManagedBlocks::new(path);

        assert_eq!(
            restarted.origin(&file, "rules").unwrap(),
            Some(Origin {
                existed: true,
                newline_added: true,
                original: None
            })
        );
        restarted.remove(&file, "rules", &[]).unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"a");
    }
}
