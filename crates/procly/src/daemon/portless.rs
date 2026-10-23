//! Portless aliases: a stable name for each Lease `up` prints as an `http://` URL, kept as long as the Lease (ADR 0011).
//!
//! Procly only ever runs `portless alias`, to make an alias and to remove one. It never starts the Portless proxy and
//! never runs `sudo`. Portless stays optional: a missing binary or a failing call is a warning, never a failure.

use std::io::{self, Read as _, Seek as _, SeekFrom};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, MutexGuard, TryLockError};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::lease::{LeaseKey, Leases, Scope};
use super::process_group::{kill_tree_blocking, lead_group_blocking};
use super::store::{self, StoreError};

/// The version of the format of the document, bumped when a field changes meaning or disappears.
const DOCUMENT_VERSION: u32 = 1;

/// The Portless executable, looked up on the `PATH` of the Daemon.
pub(crate) const BINARY: &str = "portless";

/// How long one call of `portless alias` may run before its process group is killed and the call counts as failed.
const CALL_TIMEOUT: Duration = Duration::from_secs(10);

/// Set to a number of milliseconds, it replaces [`CALL_TIMEOUT`] in the Daemon that reads it, so that tests do not wait
/// 10 seconds for a `portless` that never answers.
const TEST_TIMEOUT_ENV: &str = "PROCLY_TEST_PORTLESS_TIMEOUT_MS";

/// How much of what a failed `portless` printed on stderr is read.
const OUTPUT_LIMIT: u64 = 8192;

/// What `portless alias --remove` prints when no such alias exists, and exits with 1.
const NO_SUCH_ALIAS: &str = "No alias found";

/// The longest label of a DNS name.
const LABEL_MAX_LEN: usize = 63;

/// One alias Procly made, recorded with the Lease that holds it so that it is removed with that Lease.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Alias {
    #[serde(flatten)]
    key: LeaseKey,
    pub(crate) name: String,
    pub(crate) port: u16,
    /// Set once the Lease took another name and the removal of this one failed, so that no Lease holds the name any
    /// more and the next removal tries again.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) superseded: bool,
}

impl Alias {
    /// Whether the Lease it was made for still holds it, under this name and on this port.
    fn held(&self, leases: &Leases) -> Result<bool, StoreError> {
        Ok(!self.superseded && leases.holds(&self.key, self.port)?)
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub(super) struct AliasDocument {
    aliases: Vec<Alias>,
}

impl store::Stored for AliasDocument {
    const VERSION: u32 = DOCUMENT_VERSION;
}

/// An alias a Lease asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Wanted {
    pub(super) key: LeaseKey,
    pub(super) name: String,
    pub(super) port: u16,
}

/// Why a call of `portless` did not go through.
#[derive(Debug, thiserror::Error)]
enum CallError {
    #[error("{BINARY} is not installed, install it or set portless = false in [settings]")]
    Missing,
    #[error("`{BINARY} {args}` {reason}")]
    Failed { args: String, reason: String },
    /// Not answered in time: the next call would hang too, so the caller makes none.
    #[error("`{BINARY} {args}` did not answer within {timeout:?}")]
    TimedOut { args: String, timeout: Duration },
    /// A removal of an alias that does not exist, which is what the removal wanted.
    #[error("`{BINARY} {args}` found no such alias")]
    NoSuchAlias { args: String },
}

/// Whether a call waits for the table when another call holds it.
#[derive(Debug, Clone, Copy)]
enum Wait {
    Yes,
    No,
}

/// The aliases Procly made, kept in a document of the state directory so that they survive restarts of the Daemon.
#[derive(Debug)]
pub(crate) struct Portless {
    path: PathBuf,
    /// Read from the document on the first request, as the Leases are. `None` until then. Held while `portless` is
    /// called and the Leases are asked, so that a removal never races the alias of a Lease granted again. A call is
    /// bounded and none follows one that timed out, so it is held for one timeout at most, and the grant of a Lease
    /// never waits for it, see [`Portless::prune_if_idle`].
    aliases: Mutex<Option<Vec<Alias>>>,
}

impl Portless {
    /// The aliases stored in the document at `path`.
    pub(crate) fn new(path: PathBuf) -> Self {
        Self {
            path,
            aliases: Mutex::new(None),
        }
    }

    /// Makes the aliases `wanted`, replacing the one a Lease had under another name. It stops at the first call that
    /// fails and returns the warning telling why, the aliases made before it stay recorded. Blocking: it runs
    /// `portless` and writes the document.
    pub(super) fn alias(&self, leases: &Leases, wanted: &[Wanted]) -> Option<String> {
        self.with(Wait::Yes, |aliases| {
            let mut warning = None;
            let mut updated = aliases.to_vec();
            for want in wanted {
                match leases.holds(&want.key, want.port) {
                    Ok(true) => {}
                    Ok(false) => continue,
                    Err(error) => return (updated, Some(format!("{:#}", anyhow::Error::from(error)))),
                }
                // The name now points at this Lease, whichever one had it before.
                updated.retain(|alias| alias.name != want.name || alias.key == want.key);
                let previous = updated
                    .iter()
                    .position(|alias| alias.key == want.key && alias.name != want.name && !alias.superseded)
                    .map(|index| updated.remove(index));
                if let Some(mut previous) = previous
                    && let Err(error) = remove(&previous.name)
                {
                    warning = Some(error.to_string());
                    // Kept, so that the next removal tries again and `doctor` names it.
                    previous.superseded = true;
                    updated.push(previous);
                    // The next call would hang as long, and the table is held meanwhile.
                    if matches!(error, CallError::TimedOut { .. }) {
                        break;
                    }
                }
                if let Err(error) = call(&["alias", &want.name, &want.port.to_string()]) {
                    warning.get_or_insert_with(|| error.to_string());
                    break;
                }
                updated.retain(|alias| alias.key != want.key || alias.superseded);
                updated.push(Alias {
                    key: want.key.clone(),
                    name: want.name.clone(),
                    port: want.port,
                    superseded: false,
                });
            }
            (updated, warning)
        })
    }

    /// Removes the aliases no Lease holds any more, so that no name points to a port another Worktree may get. An
    /// alias whose removal fails stays recorded, and the next removal tries again. Returns the warning for the first
    /// failure. Blocking: it runs `portless` and writes the document.
    pub(super) fn prune(&self, leases: &Leases) -> Option<String> {
        self.prune_with(Wait::Yes, leases)
    }

    /// Like [`Portless::prune`], for the grant of a Lease, which must not wait behind a slow `portless`: when another
    /// call holds the table it does nothing, and the next removal takes what is left.
    pub(super) fn prune_if_idle(&self, leases: &Leases) -> Option<String> {
        self.prune_with(Wait::No, leases)
    }

    fn prune_with(&self, wait: Wait, leases: &Leases) -> Option<String> {
        self.with(wait, |aliases| {
            let mut warning = None;
            let mut kept = Vec::new();
            for alias in aliases {
                match alias.held(leases) {
                    Ok(true) => kept.push(alias.clone()),
                    Ok(false) => {
                        // Only the first failure is told, the next call of `portless` would fail the same way.
                        if warning.is_none() {
                            match remove(&alias.name) {
                                Ok(()) => {
                                    eprintln!("removed the Portless alias {}", alias.name);
                                    continue;
                                }
                                Err(error) => warning = Some(error.to_string()),
                            }
                        }
                        kept.push(alias.clone());
                    }
                    Err(error) => {
                        warning.get_or_insert_with(|| format!("{:#}", anyhow::Error::from(error)));
                        kept.push(alias.clone());
                    }
                }
            }
            (kept, warning)
        })
    }

    /// The aliases recorded that no Lease holds, which `portless alias --remove` removes. Blocking: it reads the
    /// documents.
    pub(super) fn unheld(&self, leases: &Leases) -> Result<Vec<Alias>, StoreError> {
        let mut table = self.table(Wait::Yes).expect("a wait always gets the table");
        let aliases = Self::loaded(&mut table, &self.path)?;
        let mut unheld = Vec::new();
        for alias in aliases {
            if !alias.held(leases)? {
                unheld.push(alias.clone());
            }
        }
        Ok(unheld)
    }

    /// The table, once no other call holds it when `wait` is [`Wait::No`], `None` if one still does.
    fn table(&self, wait: Wait) -> Option<MutexGuard<'_, Option<Vec<Alias>>>> {
        let poisoned = "the alias table mutex is never poisoned, no code panics while holding it";
        match wait {
            Wait::Yes => Some(self.aliases.lock().expect(poisoned)),
            Wait::No => match self.aliases.try_lock() {
                Ok(table) => Some(table),
                Err(TryLockError::WouldBlock) => None,
                Err(TryLockError::Poisoned(_)) => panic!("{poisoned}"),
            },
        }
    }

    /// Runs `action` on the aliases recorded, and stores what it returns when that differs. The warning it returns is
    /// logged and handed back, and a document that cannot be read or written is one too.
    fn with(&self, wait: Wait, action: impl FnOnce(&[Alias]) -> (Vec<Alias>, Option<String>)) -> Option<String> {
        let mut table = self.table(wait)?;
        let aliases = match Self::loaded(&mut table, &self.path) {
            Ok(aliases) => aliases,
            Err(error) => return Some(format!("{:#}", anyhow::Error::from(error))),
        };
        // Nothing is recorded when Portless was never on, and then `portless` is never called to remove.
        let (updated, warning) = action(aliases);
        let mut warning = warning;
        if updated != *aliases {
            let saved = store::save(
                &self.path,
                DOCUMENT_VERSION,
                &AliasDocument {
                    aliases: updated.clone(),
                },
            );
            match saved {
                Ok(()) => *aliases = updated,
                Err(error) => {
                    warning.get_or_insert_with(|| format!("{:#}", anyhow::Error::from(error)));
                }
            }
        }
        if let Some(warning) = &warning {
            eprintln!("warning: {warning}");
        }
        warning
    }

    fn loaded<'a>(table: &'a mut Option<Vec<Alias>>, path: &Path) -> Result<&'a mut Vec<Alias>, StoreError> {
        if table.is_none() {
            let document = store::load::<AliasDocument>(path, DOCUMENT_VERSION)?;
            *table = Some(document.map(|document| document.aliases).unwrap_or_default());
        }
        Ok(table.as_mut().expect("the table was read just above if it was not yet"))
    }
}

/// The alias name of an HTTP Lease of the Service `service`: `<service>`, or `<port-name>.<service>` for the Lease
/// named `port_name`, given when the Service has several. `prefix`, the branch, comes first with a dot when there is one.
pub(super) fn name(prefix: Option<&str>, service: &str, port_name: Option<&str>) -> String {
    let mut name = String::new();
    if let Some(prefix) = prefix {
        name.push_str(&label(prefix));
        name.push('.');
    }
    if let Some(port_name) = port_name {
        name.push_str(&label(port_name));
        name.push('.');
    }
    name.push_str(&label(service));
    name
}

/// What goes before the name of a Service for the Lease `scope`: nothing for the Main worktree, else the branch.
/// `branch` is the branch of the Worktree, `None` on a detached `HEAD`, where the Worktree slug stands in for it.
pub(super) fn prefix(scope: &Scope, main: &Path, branch: Option<&str>, slug: &str) -> Option<String> {
    match scope {
        Scope::Branch(name) => Some(name.clone()),
        Scope::Worktree(path) if path == main => None,
        Scope::Worktree(_) => Some(branch.unwrap_or(slug).to_owned()),
    }
}

/// `text` as a DNS label: lowercase ASCII letters and digits, any other run of characters folded into one `-`.
fn label(text: &str) -> String {
    let mut label = String::new();
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            label.push(c.to_ascii_lowercase());
        } else if !label.is_empty() && !label.ends_with('-') {
            label.push('-');
        }
    }
    // Only ASCII is pushed, so any byte index is a character boundary.
    label.truncate(LABEL_MAX_LEN);
    let label = label.trim_end_matches('-');
    if label.is_empty() {
        "worktree".to_owned()
    } else {
        label.to_owned()
    }
}

/// Removes the alias `name`. An alias that is already gone counts as removed, since Portless exits with 1 on it and a
/// record that could never be removed would be retried by every grant of a Lease for ever.
fn remove(name: &str) -> Result<(), CallError> {
    match call(&["alias", "--remove", name]) {
        Err(CallError::NoSuchAlias { .. }) => Ok(()),
        other => other,
    }
}

/// How long a call may run: [`CALL_TIMEOUT`], or what the tests set.
fn call_timeout() -> Duration {
    std::env::var(TEST_TIMEOUT_ENV)
        .ok()
        .and_then(|milliseconds| milliseconds.parse().ok())
        .map_or(CALL_TIMEOUT, Duration::from_millis)
}

/// Runs `portless` with `args`, in a process group of its own that is killed past [`call_timeout`], so that no
/// descendant outlives a call that hung. The call ends with the process it started: a descendant that keeps its
/// stderr open past that is left alone, and what it prints later is not read. Never `proxy`, never `sudo`.
fn call(args: &[&str]) -> Result<(), CallError> {
    let joined = args.join(" ");
    let failed = |reason: String| CallError::Failed {
        args: joined.clone(),
        reason,
    };
    // A file rather than a pipe, since a pipe is only at its end once every descendant closed it.
    let mut output = tempfile::tempfile().map_err(|error| failed(format!("cannot run: {error}")))?;
    let stderr = output
        .try_clone()
        .map_err(|error| failed(format!("cannot run: {error}")))?;
    let mut command = Command::new(BINARY);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr);
    lead_group_blocking(&mut command);
    let mut child = command.spawn().map_err(|error| match error.kind() {
        io::ErrorKind::NotFound => CallError::Missing,
        _ => failed(format!("cannot run: {error}")),
    })?;
    let timeout = call_timeout();
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                kill_tree_blocking(&mut child);
                return Err(CallError::TimedOut { args: joined, timeout });
            }
            Err(error) => {
                kill_tree_blocking(&mut child);
                return Err(failed(format!("cannot wait for it: {error}")));
            }
        }
    };
    if status.success() {
        return Ok(());
    }
    let mut text = Vec::new();
    // What it printed is only for the message, a file that cannot be read leaves the exit status to tell.
    if output.seek(SeekFrom::Start(0)).is_ok() {
        let _ = output.by_ref().take(OUTPUT_LIMIT).read_to_end(&mut text);
    }
    let text = String::from_utf8_lossy(&text);
    if args.get(1) == Some(&"--remove") && text.contains(NO_SUCH_ALIAS) {
        return Err(CallError::NoSuchAlias { args: joined });
    }
    let first = text.lines().map(str::trim).find(|line| !line.is_empty());
    Err(failed(match first {
        Some(line) => format!("failed: {line}"),
        None => format!("failed with {status}"),
    }))
}

/// Whether a `portless` executable is on the `PATH`, as `up` finds it.
pub(crate) fn installed() -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| is_executable(&dir.join(BINARY)))
}

fn is_executable(path: &Path) -> bool {
    path.metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_service_with_one_http_lease_is_named_after_itself_and_the_branch_comes_first_outside_the_main_worktree() {
        assert_eq!(name(None, "web", None), "web");
        assert_eq!(name(Some("fix-ui"), "web", None), "fix-ui.web");
    }

    #[test]
    fn a_service_with_several_http_leases_adds_the_port_name_before_the_service() {
        assert_eq!(name(None, "web", Some("admin")), "admin.web");
        assert_eq!(name(Some("fix-ui"), "web", Some("http")), "fix-ui.http.web");
    }

    #[test]
    fn a_name_is_made_of_dns_labels() {
        assert_eq!(name(Some("feature/E-59"), "api_v2", None), "feature-e-59.api-v2");
        assert_eq!(name(Some("///"), "web", None), "worktree.web");
        assert_eq!(label(&"a".repeat(80)).len(), LABEL_MAX_LEN);
    }

    #[test]
    fn the_main_worktree_has_no_prefix_a_pinned_branch_its_name_and_another_worktree_its_branch_or_its_slug() {
        let main = Path::new("/p/work/acme/app");
        let worktree = Scope::Worktree(PathBuf::from("/p/work/acme/app-login"));
        assert_eq!(
            prefix(&Scope::Worktree(main.to_owned()), main, Some("main"), "app"),
            None
        );
        assert_eq!(
            prefix(&Scope::Branch("develop".to_owned()), main, None, "app"),
            Some("develop".to_owned())
        );
        assert_eq!(
            prefix(&worktree, main, Some("fix-ui"), "app_login"),
            Some("fix-ui".to_owned())
        );
        assert_eq!(prefix(&worktree, main, None, "app_login"), Some("app_login".to_owned()));
    }
}
