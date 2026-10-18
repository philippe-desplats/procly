//! The Services proposed from the `dev` scripts of the `package.json` files of a Worktree.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::ops::Range;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::{Proposal, ProposalError, ServiceEntry, service_name};

/// The JavaScript package manager a Worktree installs with, which runs its scripts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Manager {
    Npm,
    Pnpm,
    Yarn,
    Bun,
}

impl Manager {
    /// Read from the lockfile the Worktree holds, `npm` when it holds none.
    fn of(worktree: &Path) -> Self {
        [
            ("pnpm-lock.yaml", Self::Pnpm),
            ("bun.lock", Self::Bun),
            ("bun.lockb", Self::Bun),
            ("yarn.lock", Self::Yarn),
        ]
        .into_iter()
        .find(|(lockfile, _)| worktree.join(lockfile).exists())
        .map_or(Self::Npm, |(_, manager)| manager)
    }

    /// The command that runs the script `script`.
    fn run(self, script: &str) -> String {
        let program = match self {
            Self::Npm => "npm",
            Self::Pnpm => "pnpm",
            Self::Yarn => "yarn",
            Self::Bun => "bun",
        };
        format!("{program} run {script}")
    }
}

/// Reads the `package.json` in `dir`, `None` when there is none.
fn read_package(dir: &Path) -> Result<Option<Value>, ProposalError> {
    let path = dir.join("package.json");
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(ProposalError::Read { path, source }),
    };
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|source| ProposalError::Json { path, source })
}

/// A package of the Worktree with a `dev` script.
#[derive(Debug)]
struct App {
    /// Where the package sits, relative to the root of the Worktree. Empty for the root itself.
    dir: PathBuf,
    name: String,
    /// The `dev` script of the package.
    script: String,
}

impl App {
    /// Reads the package in `dir`, `None` when it has no `dev` script.
    fn read(worktree: &Path, dir: PathBuf) -> Result<Option<Self>, ProposalError> {
        let Some(package) = read_package(&worktree.join(&dir))? else {
            return Ok(None);
        };
        let Some(script) = package.pointer("/scripts/dev").and_then(Value::as_str) else {
            return Ok(None);
        };
        let fallback = dir
            .file_name()
            .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
        let name = package.get("name").and_then(Value::as_str).unwrap_or(&fallback);
        Ok(Some(Self {
            name: service_name(name, &service_name(&fallback, "app")),
            script: script.to_owned(),
            dir,
        }))
    }

    /// The Service that runs the `dev` script from the root of the Worktree. When the script names a port, the Service
    /// declares it and runs the script itself with that port replaced by its placeholder, since the package manager
    /// would run the script with the literal port.
    fn service(&self, manager: Manager) -> ServiceEntry {
        let (run, ports) = match bind_port(&self.script) {
            Some((port, script)) => (
                self.in_dir(&format!("{} && {script}", self.bins())),
                BTreeMap::from([(PORT_NAME.to_owned(), port)]),
            ),
            None => (self.in_dir(&manager.run("dev")), BTreeMap::new()),
        };
        ServiceEntry {
            ports,
            ..ServiceEntry::process(run)
        }
    }

    /// `command` run from the directory of the package.
    fn in_dir(&self, command: &str) -> String {
        if self.dir.as_os_str().is_empty() {
            return command.to_owned();
        }
        format!("cd {} && {command}", shell_word(&self.dir.to_string_lossy()))
    }

    /// Puts the binaries of the package and of the root of the Worktree on the `PATH`, where a package manager would.
    fn bins(&self) -> String {
        let depth = self.dir.components().count();
        let root = if depth == 0 {
            String::new()
        } else {
            format!(":$PWD/{}node_modules/.bin", "../".repeat(depth))
        };
        format!("export PATH=\"$PWD/node_modules/.bin{root}:$PATH\"")
    }
}

/// The Port name of the port a dev script names.
const PORT_NAME: &str = "http";
/// The lowest port taken for a dev server's, so that a small number after `-p` is not read as one.
const LOWEST_PORT: u16 = 1024;

/// The port a dev script hardcodes, in `--port N`, `--port=N`, `-p N` or `PORT=N`, and the script with it replaced by
/// the placeholder of the Port name. `None` when the script names no port, or names several, since one Port name
/// cannot stand for both.
fn bind_port(script: &str) -> Option<(u16, String)> {
    let mut tokens = Vec::new();
    let mut start = None;
    for (offset, c) in script.char_indices() {
        match (c.is_whitespace(), start) {
            (true, Some(from)) => {
                tokens.push((from, &script[from..offset]));
                start = None;
            }
            (false, None) => start = Some(offset),
            _ => {}
        }
    }
    if let Some(from) = start {
        tokens.push((from, &script[from..]));
    }

    let mut found: Vec<(Range<usize>, u16)> = Vec::new();
    for (index, &(from, token)) in tokens.iter().enumerate() {
        let value = if matches!(token, "--port" | "-p") {
            tokens.get(index + 1).copied()
        } else {
            ["--port=", "PORT="]
                .iter()
                .find_map(|prefix| token.strip_prefix(prefix).map(|rest| (from + prefix.len(), rest)))
        };
        if let Some(port) = value.and_then(|(at, text)| port_in(at, text)) {
            found.push(port);
        }
    }
    let (_, port) = found.first()?;
    let port = *port;
    if found.iter().any(|(_, other)| *other != port) {
        return None;
    }
    let mut replaced = script.to_owned();
    for (range, _) in found.into_iter().rev() {
        replaced.replace_range(range, &format!("{{ports.{PORT_NAME}}}"));
    }
    Some((port, replaced))
}

/// The port that `text`, found at byte `at` of a script, holds between optional quotes, and where.
fn port_in(at: usize, text: &str) -> Option<(Range<usize>, u16)> {
    let quotes = ['"', '\''];
    let unquoted = text.trim_start_matches(quotes);
    let digits = unquoted.trim_end_matches(quotes);
    let port: u16 = digits.parse().ok().filter(|port| *port >= LOWEST_PORT)?;
    let from = at + (text.len() - unquoted.len());
    Some((from..from + digits.len(), port))
}

/// `text` as one word of a shell command.
fn shell_word(text: &str) -> String {
    if !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_./@%+=:,-".contains(c))
    {
        return text.to_owned();
    }
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// The workspace patterns of the Worktree: the `workspaces` of its root `package.json`, as a list or as a table with
/// `packages`, then the `packages` of its `pnpm-workspace.yaml`.
fn workspace_patterns(worktree: &Path, root: &Value) -> Result<Vec<String>, ProposalError> {
    let strings = |value: Option<&Value>| -> Vec<String> {
        value
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|pattern| pattern.as_str().map(str::to_owned))
            .collect()
    };
    let workspaces = root.get("workspaces");
    let mut patterns = strings(
        workspaces
            .filter(|value| value.is_array())
            .or(workspaces.and_then(|value| value.get("packages"))),
    );

    let path = worktree.join("pnpm-workspace.yaml");
    match fs::read_to_string(&path) {
        Ok(text) => {
            let yaml: serde_yaml_ng::Value =
                serde_yaml_ng::from_str(&text).map_err(|source| ProposalError::Yaml { path, source })?;
            patterns.extend(
                yaml.get("packages")
                    .and_then(serde_yaml_ng::Value::as_sequence)
                    .into_iter()
                    .flatten()
                    .filter_map(|pattern| pattern.as_str().map(str::to_owned)),
            );
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(source) => return Err(ProposalError::Read { path, source }),
    }
    Ok(patterns)
}

/// The directories, relative to the root of the Worktree, that a workspace pattern names. A segment is a directory
/// name or `*` for any directory, which is as far as the patterns of the package managers go that Procly reads.
fn expand(worktree: &Path, pattern: &str) -> Result<Vec<PathBuf>, ProposalError> {
    let mut dirs = vec![PathBuf::new()];
    for segment in pattern
        .split('/')
        .filter(|segment| !segment.is_empty() && *segment != ".")
    {
        let mut next = Vec::new();
        for dir in dirs {
            if segment == "*" {
                next.extend(subdirectories(worktree, &dir)?);
            } else if !segment.contains('*') && worktree.join(&dir).join(segment).is_dir() {
                next.push(dir.join(segment));
            }
        }
        dirs = next;
    }
    Ok(dirs)
}

/// The directories inside `dir`, hidden ones and `node_modules` left out, in name order.
fn subdirectories(worktree: &Path, dir: &Path) -> Result<Vec<PathBuf>, ProposalError> {
    let path = worktree.join(dir);
    let entries = match fs::read_dir(&path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(ProposalError::Read { path, source }),
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| ProposalError::Read {
            path: path.clone(),
            source,
        })?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with('.') && name != "node_modules" && entry.path().is_dir() {
            names.push(name);
        }
    }
    names.sort();
    Ok(names.into_iter().map(|name| dir.join(name)).collect())
}

/// The packages of the workspaces the Worktree declares that have a `dev` script, in the order of their directories.
fn workspace_apps(worktree: &Path, root: &Value) -> Result<Vec<App>, ProposalError> {
    let mut included = BTreeSet::new();
    let mut excluded = BTreeSet::new();
    for pattern in workspace_patterns(worktree, root)? {
        match pattern.strip_prefix('!') {
            Some(pattern) => excluded.extend(expand(worktree, pattern)?),
            None => included.extend(expand(worktree, &pattern)?),
        }
    }
    let mut apps = Vec::new();
    for dir in included.difference(&excluded).filter(|dir| !dir.as_os_str().is_empty()) {
        apps.extend(App::read(worktree, dir.clone())?);
    }
    Ok(apps)
}

/// Adds the Services the `dev` scripts of the Worktree rooted at `worktree` call for: one per workspace package with
/// a `dev` script, and a Group for the `dev` script of the root that covers them, or, without workspaces, one for the
/// root package.
pub(super) fn propose(worktree: &Path, proposal: &mut Proposal) -> Result<(), ProposalError> {
    let Some(root) = read_package(worktree)? else {
        return Ok(());
    };
    let manager = Manager::of(worktree);
    let apps = workspace_apps(worktree, &root)?;
    let root_app = App::read(worktree, PathBuf::new())?;
    if apps.is_empty() {
        if let Some(app) = root_app {
            proposal.add(&app.name, app.service(manager));
        }
        return Ok(());
    }
    let mut covers: Vec<String> = apps
        .iter()
        .map(|app| proposal.add(&app.name, app.service(manager)))
        .collect();
    if root_app.is_some() {
        covers.sort();
        // The Group runs the root script as it is: the apps it covers get their ports from their own Leases.
        proposal.add(
            "dev",
            ServiceEntry {
                covers: Some(covers),
                ..ServiceEntry::process(manager.run("dev"))
            },
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bound(script: &str) -> Option<(u16, String)> {
        bind_port(script)
    }

    #[test]
    fn a_port_after_a_flag_or_an_assignment_is_replaced_by_the_placeholder() {
        for (script, port, replaced) in [
            ("next dev --port 3001", 3001, "next dev --port {ports.http}"),
            ("vite --port=5173 --host", 5173, "vite --port={ports.http} --host"),
            ("serve -p 4000 .", 4000, "serve -p {ports.http} ."),
            ("PORT=3333 node server.js", 3333, "PORT={ports.http} node server.js"),
            (
                "cross-env PORT=\"1080\" mailer",
                1080,
                "cross-env PORT=\"{ports.http}\" mailer",
            ),
            (
                "a --port 3000 && b --port 3000",
                3000,
                "a --port {ports.http} && b --port {ports.http}",
            ),
        ] {
            assert_eq!(bound(script), Some((port, replaced.to_owned())), "{script}");
        }
    }

    #[test]
    fn a_script_without_exactly_one_port_above_the_lowest_is_left_alone() {
        for script in [
            "node server.js",
            "a --port 3000 & b --port 3001",
            "jest -p 2",
            "tsc -p tsconfig.json",
            "next dev --port",
            "serve --port 3000;",
            "NODE_PORT=3000 node server.js",
            "vite --port 70000",
        ] {
            assert_eq!(bound(script), None, "{script}");
        }
    }

    #[test]
    fn a_directory_is_one_shell_word() {
        assert_eq!(shell_word("apps/web"), "apps/web");
        assert_eq!(shell_word("apps/my app"), "'apps/my app'");
        assert_eq!(shell_word("apps/it's"), "'apps/it'\\''s'");
    }

    #[test]
    fn the_binaries_of_the_package_and_of_the_root_are_put_on_the_path() {
        let app = |dir: &str| App {
            dir: PathBuf::from(dir),
            name: "x".to_owned(),
            script: String::new(),
        };
        assert_eq!(app("").bins(), "export PATH=\"$PWD/node_modules/.bin:$PATH\"");
        assert_eq!(
            app("apps/web").bins(),
            "export PATH=\"$PWD/node_modules/.bin:$PWD/../../node_modules/.bin:$PATH\""
        );
    }

    #[test]
    fn the_package_manager_is_read_from_the_lockfile() {
        let worktree = tempfile::tempdir().unwrap();
        assert_eq!(Manager::of(worktree.path()), Manager::Npm);
        for (lockfile, manager) in [
            ("yarn.lock", Manager::Yarn),
            ("bun.lockb", Manager::Bun),
            ("bun.lock", Manager::Bun),
            ("pnpm-lock.yaml", Manager::Pnpm),
        ] {
            fs::write(worktree.path().join(lockfile), "").unwrap();
            assert_eq!(Manager::of(worktree.path()), manager, "{lockfile}");
        }
    }

    #[test]
    fn a_workspace_pattern_names_directories_by_name_or_with_a_star() {
        let worktree = tempfile::tempdir().unwrap();
        for dir in ["apps/web", "apps/api", "apps/.hidden", "apps/node_modules", "tools/cli"] {
            fs::create_dir_all(worktree.path().join(dir)).unwrap();
        }
        let dirs = |pattern: &str| expand(worktree.path(), pattern).unwrap();
        assert_eq!(dirs("apps/*"), [PathBuf::from("apps/api"), PathBuf::from("apps/web")]);
        assert_eq!(dirs("./tools/cli/"), [PathBuf::from("tools/cli")]);
        assert_eq!(dirs("*/web"), [PathBuf::from("apps/web")]);
        assert_eq!(dirs("missing/*"), Vec::<PathBuf>::new());
        assert_eq!(
            dirs("apps/w*"),
            Vec::<PathBuf>::new(),
            "only whole-segment stars are read"
        );
    }
}
