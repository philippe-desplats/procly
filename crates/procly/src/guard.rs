//! The Guard, as the Claude Code hook `procly guard` runs it: which Bash commands start a dev server or containers
//! that Procly runs instead (see docs/agents.md for the list).
//!
//! The recognition happens in the client and not in the Daemon, since the hook runs before every Bash command of the
//! agent and a command the Guard does not know must neither wait for the Daemon nor start it. Only a command it
//! knows asks the Daemon whether the Repository is Off.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::protocol::GuardService;
use crate::shell::{self, Resolved, basename, positional, segments, strip_launchers};

/// What a recognized command starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    DevServer,
    Compose,
}

/// A command the Guard blocks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Recognized {
    pub(crate) kind: Kind,
    /// The command that matched, as typed, without the `cd` or the variables around it.
    pub(crate) command: String,
    /// Where the last `cd` or `pushd` before it moved, `None` when the command runs where the command line was run.
    pub(crate) moved_to: Option<String>,
}

/// The Bash tool call a `PreToolUse` hook receives on its standard input.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ToolCall {
    pub(crate) command: String,
    /// The working directory of the agent, which a `cd` of an earlier command may have moved.
    pub(crate) cwd: Option<PathBuf>,
}

/// The Bash call in `input`, `None` for any other tool and for input that is not a tool call.
pub(crate) fn tool_call(input: &str) -> Option<ToolCall> {
    let call: Value = serde_json::from_str(input).ok()?;
    if call["tool_name"] != "Bash" {
        return None;
    }
    Some(ToolCall {
        command: call["tool_input"]["command"].as_str()?.to_owned(),
        cwd: call["cwd"].as_str().map(PathBuf::from),
    })
}

/// The answer that makes Claude Code refuse the call and tell the agent why, with the Procly command to run instead:
/// the one of the declared Service the blocked command starts, `procly up` for a dev command of the whole Repository
/// run at the root of its Worktree, or else `procly up <service>` for the agent to pick among `services`. `directory`
/// is where the command line was run, relative to the root of the Worktree, `None` when it is not known, and `root` is
/// the root of the Worktree itself, which a compose file given with an absolute path is placed against.
/// `compose_file` is the compose file `settings.compose_file` declares, relative to `root`, `None` when the Worktree
/// declares none and Procly runs the compose file at its root.
pub(crate) fn denial(
    recognized: &Recognized,
    services: &[GuardService],
    directory: Option<&Path>,
    root: &Path,
    compose_file: Option<&Path>,
) -> String {
    let Recognized { kind, command, .. } = recognized;
    let why = match kind {
        Kind::DevServer => {
            "starts a dev server. This repository runs its dev servers through Procly, so that parallel worktrees \
             never start the same one twice or fight over a port."
        }
        Kind::Compose => {
            "starts containers. This repository runs them through Procly, which shares one container \
             between its worktrees."
        }
    };
    let instead = match (equivalent(recognized, services, directory, root, compose_file), kind) {
        (Some(equivalent), _) => format!("Run `{equivalent}` instead."),
        (None, Kind::Compose)
            if let Some(Unmanaged { typed, placed }) =
                unmanaged_compose_file(recognized, directory, root, compose_file) =>
        {
            // Nothing else to say: the Services of the managed compose file would read as a way to start this one.
            let reason = match (compose_file, placed) {
                (Some(declared), _) => format!(
                    "The compose file `{typed}` is not managed by Procly, which runs `{}`, the compose file \
                     `compose_file` declares in the [settings] of procly.toml, so no Procly command starts it.",
                    declared.display()
                ),
                (None, Some(placed)) => format!(
                    "The compose file `{typed}` is not managed by Procly, which runs the compose file at the root of \
                     the Worktree unless one is declared. Declare it with `compose_file = \"{}\"` in the [settings] \
                     of procly.toml, and its services as Compose services, for Procly to start them.",
                    placed.display()
                ),
                (None, None) => format!(
                    "The compose file `{typed}` is not managed by Procly, which runs the compose file at the root of \
                     the Worktree or the one `compose_file` declares in the [settings] of procly.toml, inside the \
                     Worktree, so no Procly command starts it."
                ),
            };
            return answer(&format!("`{command}` {why} {reason}"));
        }
        (None, Kind::DevServer) => "Run `procly up <service>` instead.".to_owned(),
        (None, Kind::Compose) => "Run `procly up <service>` for the Compose service instead.".to_owned(),
    };
    let declared = if services.is_empty() {
        "No Service is declared in procly.toml yet, declare it there first.".to_owned()
    } else {
        let names: Vec<&str> = services.iter().map(|service| service.name.as_str()).collect();
        format!("Declared Services: {}.", names.join(", "))
    };
    answer(&format!("`{command}` {why} {instead} {declared}"))
}

/// The answer that makes Claude Code refuse the call, with `reason` for the agent.
fn answer(reason: &str) -> String {
    json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason,
        }
    })
    .to_string()
}

/// The Procly command that does what the blocked command does, `None` when nothing declared tells. `directory` is where
/// the command line was run, relative to the root of the Worktree, `root` is the root itself, and `compose_file` the
/// compose file `settings.compose_file` declares.
fn equivalent(
    recognized: &Recognized,
    services: &[GuardService],
    directory: Option<&Path>,
    root: &Path,
    compose_file: Option<&Path>,
) -> Option<String> {
    let blocked = words_of(&recognized.command);
    let blocked = strip_launchers(&blocked);
    match recognized.kind {
        Kind::DevServer => {
            // Run from the same directory, so that `cd apps/api && pnpm dev` is never the `pnpm dev` of `apps/web`,
            // and `pnpm dev` run in `apps/web` is the Service that changes to it.
            let place = place_of(recognized, directory, root);
            let matches: Vec<(usize, &GuardService)> = services
                .iter()
                .filter_map(|service| {
                    let run = recognize(service.run.as_deref()?)?;
                    if place.is_none() || place != place_of(&run, Some(Path::new("")), root) {
                        return None;
                    }
                    let matched = same_start(&compared(blocked), &compared(strip_launchers(&words_of(&run.command))))?;
                    Some((matched, service))
                })
                .collect();
            // The longest match wins. Services matching as long as each other leave the agent to pick.
            let longest = matches.iter().map(|(matched, _)| *matched).max();
            let mut best = matches.iter().filter(|(matched, _)| Some(*matched) == longest);
            match (best.next(), best.next()) {
                (Some((_, service)), None) => Some(format!("procly up {}", service.name)),
                (Some(_), Some(_)) => None,
                (None, _) => (place.as_deref() == Some(Path::new("")) && runs_the_whole_repository(blocked))
                    .then(|| "procly up".to_owned()),
            }
        }
        Kind::Compose => {
            if unmanaged_compose_file(recognized, directory, root, compose_file).is_some() {
                return None;
            }
            let named = compose_services(blocked);
            let started: Vec<String> = services
                .iter()
                .filter(|service| {
                    service
                        .compose
                        .as_ref()
                        .is_some_and(|compose| named.is_empty() || named.contains(&compose.as_str()))
                })
                .map(|service| format!("procly up {}", service.name))
                .collect();
            (!started.is_empty()).then(|| started.join(" && "))
        }
    }
}

/// The compose files a compose command is given with `-f` or `--file`, in the order given.
fn compose_files(words: &[String]) -> Vec<&str> {
    let mut files = Vec::new();
    let mut words = after_compose(words).iter().map(String::as_str);
    while let Some(word) = words.next() {
        if matches!(word, "-f" | "--file") {
            files.extend(words.next());
        } else if let Some(file) = word.strip_prefix("--file=") {
            files.push(file);
        } else if let Some(file) = word
            .strip_prefix("-f")
            .filter(|file| !file.is_empty() && !word.starts_with("--"))
        {
            // `-fdev.yml` and `-f=dev.yml`, which the option parser of Docker Compose reads as `-f dev.yml`.
            files.push(file.strip_prefix('=').unwrap_or(file));
        } else if COMPOSE_OPTIONS_WITH_VALUE.contains(&word) {
            words.next();
        } else if !word.starts_with('-') {
            // The subcommand, after which an option is no longer one of compose.
            break;
        }
    }
    files
}

/// A compose file Procly does not manage, which a compose command runs.
#[derive(Debug, PartialEq, Eq)]
struct Unmanaged {
    /// As typed, or as found from where the command runs when the command names none.
    typed: String,
    /// Relative to the root of the Worktree, `None` when it is not inside it.
    placed: Option<PathBuf>,
}

/// The compose file a compose command runs that Procly does not manage, `None` when every file it is given is the
/// managed one, and when a file cannot be placed from where it is written. Procly drives one compose file only
/// (ADR 0004): the one `compose_file` declares, relative to `root`, or else the one at the root of the Worktree. A
/// command that is given no file runs the compose file Docker Compose finds from where it runs, in that directory or
/// the nearest one above it up to the root, and the managed one when there is none to find. A file given with an
/// absolute path is placed against `root`, the root of the Worktree: the managed file named that way is managed, any
/// other absolute path, inside the Worktree or not, is not.
fn unmanaged_compose_file(
    recognized: &Recognized,
    directory: Option<&Path>,
    root: &Path,
    compose_file: Option<&Path>,
) -> Option<Unmanaged> {
    let place = place_of(recognized, directory, root);
    let blocked = words_of(&recognized.command);
    let managed = |resolved: &Path| match compose_file {
        Some(declared) => {
            resolve(Path::new(""), &declared.to_string_lossy()).is_some_and(|declared| declared == resolved)
        }
        None => COMPOSE_FILES.iter().any(|name| resolved == Path::new(name)),
    };
    let given = compose_files(strip_launchers(&blocked));
    if given.is_empty() {
        let found = found_compose_file(place.as_deref()?, root)?;
        return (!managed(&found)).then(|| Unmanaged {
            typed: found.display().to_string(),
            placed: Some(found),
        });
    }
    given.into_iter().find_map(|file| {
        let resolved = if file.starts_with('/') {
            Some(within(root, file))
        } else {
            place.as_deref().and_then(|place| resolve(place, file))
        }?;
        (!managed(&resolved)).then(|| Unmanaged {
            typed: file.to_owned(),
            placed: (resolved.is_relative() && !resolved.starts_with("..")).then_some(resolved),
        })
    })
}

/// The compose file Docker Compose runs from `place`, relative to `root`, when it is given none: the first of
/// [`COMPOSE_FILES`] in `place`, or else in the nearest directory above it, up to the root of the Worktree. `None` when
/// there is none, and when `place` is not inside the Worktree.
fn found_compose_file(place: &Path, root: &Path) -> Option<PathBuf> {
    if place.starts_with("..") || place.is_absolute() {
        return None;
    }
    let mut directory = Some(place);
    while let Some(here) = directory {
        if let Some(name) = COMPOSE_FILES.iter().find(|name| root.join(here).join(name).is_file()) {
            return Some(here.join(name));
        }
        directory = here.parent();
    }
    None
}

/// The compose files Docker Compose looks for, in the order it prefers them, which Procly drives when the Worktree has
/// one.
pub(crate) const COMPOSE_FILES: [&str; 4] = [
    "compose.yaml",
    "compose.yml",
    "docker-compose.yaml",
    "docker-compose.yml",
];

/// Where the recognized command runs, relative to the root of the Worktree: `directory` moved by the `cd` before it,
/// then by a directory option of its package manager, such as `--cwd apps/web`. An absolute path is placed against
/// `root`, the root of the Worktree. `None` when the directory is not known and the command is not moved by an
/// absolute path, and when it is moved out of the Worktree or to a place only the home directory tells.
fn place_of(recognized: &Recognized, directory: Option<&Path>, root: &Path) -> Option<PathBuf> {
    let place = moved(directory, recognized.moved_to.as_deref().unwrap_or("."), root)?;
    match directory_option(strip_launchers(&words_of(&recognized.command))) {
        Some(option) => moved(Some(&place), option, root),
        None => Some(place),
    }
}

/// The directory of a Service whose command is `run`, relative to `root`, the root of its Worktree: where the `cd` or
/// `pushd` commands at the head of `run` lead, placed as the Guard places a command, and the root itself when `run`
/// starts with none. `None` when they lead out of the Worktree or to a place only the home directory tells.
pub(crate) fn service_directory(run: &str, root: &Path) -> Option<PathBuf> {
    let mut place = PathBuf::new();
    for segment in segments(run) {
        let words = strip_launchers(&segment.words);
        if !matches!(words.first().map(|word| basename(word)), Some("cd" | "pushd")) {
            break;
        }
        place = moved(Some(&place), words.get(1).map_or("~", String::as_str), root)?;
    }
    (!place.starts_with("..")).then_some(place)
}

/// Where `path` leads from `directory`, both relative to `root`, the root of the Worktree. An absolute `path` is placed
/// with [`within`], and when that leaves it outside `root`, again on the canonical paths of both, so that a symlink to
/// the Worktree and the `/tmp` that is `/private/tmp` lead into it. `None` for an absolute path that is not inside
/// `root` either way, and for a relative one when `directory` is not known.
fn moved(directory: Option<&Path>, path: &str, root: &Path) -> Option<PathBuf> {
    if !path.starts_with('/') {
        return resolve(directory?, path);
    }
    let placed = within(root, path);
    if placed.is_relative() {
        return Some(placed);
    }
    let canonical = placed.canonicalize().ok()?;
    canonical
        .strip_prefix(root.canonicalize().ok()?)
        .ok()
        .map(Path::to_owned)
}

/// The options of a package manager that run it in another directory, as a `cd` to it would.
const DIRECTORY_OPTIONS: [&str; 4] = ["--cwd", "--prefix", "--dir", "-C"];

/// The directory a package manager is told to run in by a directory option, written `--cwd apps/web` or
/// `--cwd=apps/web`, the last one when it is given several. `None` for any other program and when it is given none.
/// What follows `--` is for the script, not for the package manager.
fn directory_option(words: &[String]) -> Option<&str> {
    let (program, rest) = words.split_first()?;
    if !RUNNERS.contains(&basename(program)) {
        return None;
    }
    let mut directory = None;
    let mut words = rest.iter().map(String::as_str).take_while(|word| *word != "--");
    while let Some(word) = words.next() {
        if DIRECTORY_OPTIONS.contains(&word) {
            directory = words.next().or(directory);
        } else if let Some((option, value)) = word.split_once('=')
            && DIRECTORY_OPTIONS.contains(&option)
        {
            directory = Some(value);
        }
    }
    directory
}

/// `words` without the directory option of their package manager and its value, which [`place_of`] reads as where the
/// command runs.
fn without_directory_option(words: &[String]) -> Vec<String> {
    if directory_option(words).is_none() {
        return words.to_vec();
    }
    let mut kept = Vec::with_capacity(words.len());
    let mut words = words.iter();
    while let Some(word) = words.next() {
        if word == "--" {
            kept.push(word.clone());
            kept.extend(words.by_ref().cloned());
        } else if DIRECTORY_OPTIONS.contains(&word.as_str()) {
            words.next();
        } else if !word
            .split_once('=')
            .is_some_and(|(option, _)| DIRECTORY_OPTIONS.contains(&option))
        {
            kept.push(word.clone());
        }
    }
    kept
}

/// `path` as written from `base`, a directory relative to the root of the Worktree, with its `.` and `..` resolved. A
/// path that climbs above the root keeps its leading `..`, so that it matches nothing inside. `None` for a path that
/// starts at `/` or at `~`, which says nothing about where the Worktree is.
fn resolve(base: &Path, path: &str) -> Option<PathBuf> {
    if path.starts_with('~') {
        return None;
    }
    match shell::resolve(base, path) {
        Resolved::Inside(resolved) | Resolved::Above(resolved) => Some(resolved),
        Resolved::Absolute(_) => None,
    }
}

/// The absolute `path`, with its `.` and `..` resolved, relative to `root` when it is inside it. A path outside `root`
/// stays absolute, so that it matches nothing inside. `path` must start with `/`, a relative one is returned as written.
fn within(root: &Path, path: &str) -> PathBuf {
    let Resolved::Absolute(absolute) = shell::resolve(Path::new(""), path) else {
        debug_assert!(false, "within takes an absolute path, got {path:?}");
        return PathBuf::from(path);
    };
    absolute.strip_prefix(root).map_or(absolute.clone(), Path::to_owned)
}

/// The words of a command that say what it starts, wherever it runs: `bun --cwd apps/web run dev` is `bun dev`.
fn compared(words: &[String]) -> Vec<String> {
    without_run(&without_directory_option(words))
}

/// `words` without the `run` of a package manager, which `bun run dev` and `bun dev` both run the script `dev` with.
fn without_run(words: &[String]) -> Vec<String> {
    let mut words = words.to_vec();
    if words
        .first()
        .is_some_and(|program| PACKAGE_MANAGERS.contains(&basename(program)))
        && words.get(1).is_some_and(|word| word == "run")
    {
        words.remove(1);
    }
    words
}

/// The words of the first simple command of `command`.
fn words_of(command: &str) -> Vec<String> {
    segments(command)
        .into_iter()
        .next()
        .map(|segment| segment.words)
        .unwrap_or_default()
}

/// How many words `blocked` and `run` share from their start, when one of them is the start of the other, a
/// `{ports.<name>}` of `run` standing for any port.
fn same_start(blocked: &[String], run: &[String]) -> Option<usize> {
    let shared = blocked.len().min(run.len());
    let same = blocked
        .iter()
        .zip(run)
        .all(|(blocked, run)| blocked == run || run.contains("{ports."));
    (same && shared > 0).then_some(shared)
}

/// The package managers whose options select a package, and whose dev script, run at the root of a Repository, runs the
/// dev servers of all of it.
pub(crate) const RUNNERS: [&str; 5] = ["npm", "pnpm", "yarn", "bun", "turbo"];

/// The package managers that run a script of a package, `npm run dev` and `bun dev`, which `concurrently` writes
/// `npm:dev`.
pub(crate) const PACKAGE_MANAGERS: [&str; 4] = ["npm", "pnpm", "yarn", "bun"];

/// The options of a package manager that narrow a command to one package of a monorepo.
pub(crate) const PACKAGE_OPTIONS: [&str; 8] = [
    "--workspace",
    "-w",
    "--prefix",
    "--filter",
    "-F",
    "--dir",
    "-C",
    "--cwd",
];

/// The options of [`PACKAGE_OPTIONS`] that take a value, every one but `-w`, which takes none in pnpm.
pub(crate) const PACKAGE_OPTIONS_WITH_VALUE: [&str; 7] =
    ["--workspace", "--prefix", "--filter", "-F", "--dir", "-C", "--cwd"];

/// Whether `words` run the dev script of a whole Repository: `pnpm dev`, `npm start`, `turbo run dev`, with no
/// option narrowing it to one package, written `--filter web` or `--filter=web`, and a plain script name.
pub(crate) fn runs_the_whole_repository(words: &[String]) -> bool {
    let Some((program, rest)) = words.split_first() else {
        return false;
    };
    let narrows = |word: &String| PACKAGE_OPTIONS.contains(&word.split('=').next().unwrap_or_default());
    if !RUNNERS.contains(&basename(program)) || rest.iter().any(narrows) {
        return false;
    }
    let positional = positional(rest, &[]);
    let script = script_after_run(&positional).or_else(|| positional.first().copied());
    script.is_some_and(|script| matches!(script, "dev" | "start" | "serve" | "develop"))
}

/// The words of a compose command after `compose`, or after `docker-compose`.
fn after_compose(words: &[String]) -> &[String] {
    match words.first().map(|program| basename(program)) {
        Some("docker") => words
            .iter()
            .position(|word| word == "compose")
            .map_or(&[][..], |at| &words[at + 1..]),
        _ => words.get(1..).unwrap_or_default(),
    }
}

/// The services of the compose file `docker compose up` names, empty when it names none and starts them all.
fn compose_services(words: &[String]) -> Vec<&str> {
    positional(after_compose(words), &COMPOSE_OPTIONS_WITH_VALUE)
        .into_iter()
        .skip_while(|word| !matches!(*word, "up" | "start"))
        .skip(1)
        .collect()
}

/// The first command of `command_line` that starts a dev server or containers, `None` when none does.
pub(crate) fn recognize(command_line: &str) -> Option<Recognized> {
    let mut moved_to: Option<String> = None;
    segments(command_line).into_iter().find_map(|segment| {
        let words = strip_launchers(&segment.words);
        if matches!(words.first().map(|word| basename(word)), Some("cd" | "pushd")) {
            let to = words.get(1).map_or("~", String::as_str);
            moved_to = Some(to.trim_start_matches("./").trim_end_matches('/').to_owned());
            return None;
        }
        // A shell given a command line to run: `bash -c 'npm run dev'`, `cross-env-shell NODE_ENV=dev "next dev"`.
        if let Some(inner) = shell::inner_command_line(&segment.words) {
            return recognize(&inner).map(|recognized| Recognized {
                moved_to: recognized.moved_to.or_else(|| moved_to.clone()),
                ..recognized
            });
        }
        classify(words).map(|kind| Recognized {
            kind,
            command: segment.text.clone(),
            moved_to: moved_to.clone(),
        })
    })
}

/// Whether a package script name is one that serves: `dev`, `start`, `serve`, `develop`, or one of them with a suffix
/// such as `dev:web`.
fn serves(script: &str) -> bool {
    matches!(script.split(':').next(), Some("dev" | "start" | "serve" | "develop"))
}

/// Dev servers started by a program and the words that follow it: `next dev` is `("next", ["dev"])`.
const DEV_SERVERS: &[(&str, &[&str])] = &[
    ("next", &["dev"]),
    ("next", &["start"]),
    ("nuxt", &["dev"]),
    ("nuxi", &["dev"]),
    ("astro", &["dev"]),
    ("astro", &["preview"]),
    ("remix", &["dev"]),
    ("webpack", &["serve"]),
    ("tsx", &["watch"]),
    ("ng", &["serve"]),
    ("gatsby", &["develop"]),
    ("hugo", &["server"]),
    ("jekyll", &["serve"]),
    ("ember", &["serve"]),
    ("rails", &["server"]),
    ("rails", &["s"]),
    ("flask", &["run"]),
    ("php", &["artisan", "serve"]),
    ("node", &["ace", "serve"]),
    ("adonis", &["serve"]),
    ("python", &["manage.py", "runserver"]),
    ("django-admin", &["runserver"]),
];

/// Programs that are a dev server whatever follows them.
const ALWAYS_DEV_SERVERS: &[&str] = &[
    "nodemon",
    "ts-node-dev",
    "webpack-dev-server",
    "uvicorn",
    "gunicorn",
    "hypercorn",
];

/// What the command whose words are `words` starts, if it is something Procly runs instead.
fn classify(words: &[String]) -> Option<Kind> {
    let program = basename(words.first()?);
    let rest = &words[1..];
    // `python3.12` is `python`.
    let program = if program.starts_with("python") {
        "python"
    } else {
        program
    };

    if ALWAYS_DEV_SERVERS.contains(&program) {
        return Some(Kind::DevServer);
    }
    match program {
        "npm" | "pnpm" | "yarn" | "bun" | "turbo" => {
            let positional = positional(rest, &PACKAGE_OPTIONS_WITH_VALUE);
            let script = match (program, positional.as_slice()) {
                ("npm", ["start", ..]) => Some("start"),
                ("npm", [..]) => script_after_run(&positional),
                ("yarn", ["workspace", _, script, ..]) => Some(*script),
                (_, [..]) => script_after_run(&positional).or_else(|| positional.first().copied()),
            };
            return script.filter(|script| serves(script)).map(|_| Kind::DevServer);
        }
        "nx" => {
            return positional(rest, &[])
                .first()
                .filter(|task| matches!(**task, "serve" | "dev"))
                .map(|_| Kind::DevServer);
        }
        "vite" => {
            // `vite` alone serves, and so do its flags. `build` and `optimize` do something else.
            let asks_for_help = rest
                .iter()
                .any(|word| matches!(word.as_str(), "--version" | "-v" | "--help" | "-h"));
            let builds = matches!(positional(rest, &[]).first(), Some(&("build" | "optimize")));
            return (!asks_for_help && !builds).then_some(Kind::DevServer);
        }
        "docker-compose" => return compose(rest),
        "docker" => {
            let positional_start = rest.iter().position(|word| word == "compose")?;
            let before = positional(&rest[..positional_start], &DOCKER_OPTIONS_WITH_VALUE);
            return before
                .is_empty()
                .then(|| compose(&rest[positional_start + 1..]))
                .flatten();
        }
        _ => {}
    }
    let positional = positional(rest, &[]);
    DEV_SERVERS
        .iter()
        .any(|(name, leading)| *name == program && positional.starts_with(leading))
        .then_some(Kind::DevServer)
}

/// The script a package manager runs: the one after `run`, or after `run-script`.
fn script_after_run<'a>(positional: &[&'a str]) -> Option<&'a str> {
    match positional {
        ["run" | "run-script", script, ..] => Some(script),
        _ => None,
    }
}

const DOCKER_OPTIONS_WITH_VALUE: [&str; 6] = ["--context", "-c", "--host", "-H", "--config", "--log-level"];

const COMPOSE_OPTIONS_WITH_VALUE: [&str; 9] = [
    "-f",
    "--file",
    "-p",
    "--project-name",
    "--profile",
    "--env-file",
    "--project-directory",
    "--ansi",
    "--progress",
];

/// Whether a `docker compose` command, given the words after `compose`, starts containers.
fn compose(words: &[String]) -> Option<Kind> {
    let positional = positional(words, &COMPOSE_OPTIONS_WITH_VALUE);
    matches!(positional.first(), Some(&("up" | "start"))).then_some(Kind::Compose)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The root of the Worktree the tests run in.
    fn root() -> &'static Path {
        Path::new("/p/work/acme/web")
    }

    fn kind(command: &str) -> Option<Kind> {
        recognize(command).map(|recognized| recognized.kind)
    }

    #[test]
    fn package_scripts_that_serve_are_dev_servers_through_every_package_manager() {
        for command in [
            "npm start",
            "npm run dev",
            "npm run-script serve",
            "npm run dev:web --workspace=web",
            "npm --workspace web run dev",
            "pnpm dev",
            "pnpm run start",
            "pnpm -C apps/web dev",
            "pnpm --filter web dev",
            "yarn dev",
            "yarn run develop",
            "yarn workspace web dev",
            "bun dev",
            "bun run dev",
            "bun run --filter web dev",
            "turbo dev",
            "turbo run dev --filter=web",
        ] {
            assert_eq!(kind(command), Some(Kind::DevServer), "{command}");
        }
    }

    #[test]
    fn the_package_options_that_take_a_value_are_all_of_them_but_the_one_that_takes_none() {
        assert!(
            PACKAGE_OPTIONS_WITH_VALUE
                .iter()
                .all(|option| PACKAGE_OPTIONS.contains(option))
        );
        let without_value: Vec<&str> = PACKAGE_OPTIONS
            .into_iter()
            .filter(|option| !PACKAGE_OPTIONS_WITH_VALUE.contains(option))
            .collect();
        assert_eq!(without_value, ["-w"]);
    }

    #[test]
    fn a_dev_server_is_recognized_behind_a_directory_option_written_either_way() {
        for command in [
            "bun --cwd apps/web run dev",
            "bun run --cwd apps/web dev",
            "bun --cwd=apps/web dev",
            "yarn --cwd apps/web dev",
            "yarn --cwd=apps/web dev",
            "npm --prefix apps/web run dev",
            "npm --prefix=apps/web start",
            "pnpm --dir apps/web dev",
            "pnpm --dir=apps/web dev",
            "pnpm -C=apps/web dev",
            "pnpm --filter=web dev",
            "turbo --cwd apps/web dev",
            "turbo dev --filter=@acme/web",
        ] {
            assert_eq!(kind(command), Some(Kind::DevServer), "{command}");
        }
        for command in [
            "bun --cwd apps/web run build",
            "yarn --cwd apps/web install",
            "npm --prefix apps/web test",
            "pnpm -C=apps/web build",
        ] {
            assert_eq!(kind(command), None, "{command}");
        }
    }

    #[test]
    fn package_scripts_that_do_something_else_are_not() {
        for command in [
            "npm test",
            "npm run build",
            "npm install",
            "npm run lint -- --fix",
            "pnpm install",
            "pnpm run test:unit",
            "pnpm --filter web build",
            "yarn",
            "yarn add left-pad",
            "bun test",
            "bun run build",
            "bun install",
            "turbo build",
            "turbo run lint",
        ] {
            assert_eq!(kind(command), None, "{command}");
        }
    }

    #[test]
    fn frameworks_and_servers_are_recognized_by_their_own_commands() {
        for command in [
            "next dev",
            "next dev -p 3001",
            "./node_modules/.bin/next start",
            "nuxt dev",
            "astro dev",
            "vite",
            "vite --port 3000",
            "vite dev",
            "nodemon server.js",
            "node ace serve --hmr",
            "node --watch ace serve",
            "rails server",
            "bin/rails s",
            "bundle exec rails s -p 3001",
            "flask run",
            "python manage.py runserver",
            "python3.12 manage.py runserver 0.0.0.0:8000",
            "php artisan serve",
            "uvicorn app:app --reload",
            "nx serve web",
        ] {
            assert_eq!(kind(command), Some(Kind::DevServer), "{command}");
        }
        for command in [
            "next build",
            "vite build",
            "rails db:migrate",
            "rails console",
            "flask db upgrade",
            "python manage.py migrate",
            "node script.js",
            "php artisan migrate",
            "nx build web",
        ] {
            assert_eq!(kind(command), None, "{command}");
        }
    }

    #[test]
    fn docker_compose_that_starts_containers_is_recognized_with_its_options_before_the_subcommand() {
        for command in [
            "docker compose up",
            "docker compose up -d db",
            "docker compose -f compose.dev.yml up",
            "docker compose --profile dev --project-name x start",
            "docker --context remote compose up",
            "docker-compose up",
            "docker-compose -f a.yml -f b.yml up -d",
        ] {
            assert_eq!(kind(command), Some(Kind::Compose), "{command}");
        }
        for command in [
            "docker compose ps",
            "docker compose logs -f web",
            "docker compose down",
            "docker compose run --rm web npm test",
            "docker compose -f up.yml ps",
            "docker ps",
            "docker run -p 3000:3000 app",
            "docker-compose config",
        ] {
            assert_eq!(kind(command), None, "{command}");
        }
    }

    #[test]
    fn what_launches_a_command_does_not_hide_it() {
        for (command, expected) in [
            ("PORT=3000 npm run dev", Kind::DevServer),
            ("cd apps/web && npm run dev", Kind::DevServer),
            ("npm run build; npm run dev", Kind::DevServer),
            ("(cd web && npm run dev)", Kind::DevServer),
            ("nohup npm run dev > dev.log 2>&1 &", Kind::DevServer),
            ("FOO=1 BAR=2 env nohup next dev", Kind::DevServer),
            ("npx next dev", Kind::DevServer),
            ("npx -y vite", Kind::DevServer),
            ("pnpm exec next dev", Kind::DevServer),
            ("bunx --bun astro dev", Kind::DevServer),
            ("bash -c 'npm run dev'", Kind::DevServer),
            ("sh -lc \"cd web && pnpm dev\"", Kind::DevServer),
            ("echo started; docker compose up -d", Kind::Compose),
            ("echo $(npm run dev)", Kind::DevServer),
            ("true |\n  npm run dev", Kind::DevServer),
            ("if ready; then npm run dev; fi", Kind::DevServer),
            ("pnpm -w dev", Kind::DevServer),
        ] {
            assert_eq!(kind(command), Some(expected), "{command}");
        }
    }

    #[test]
    fn a_wrapper_that_loads_variables_does_not_hide_the_dev_command_it_runs() {
        for command in [
            "cross-env FORCE_COLOR=1 turbo run dev --parallel",
            "npx cross-env FORCE_COLOR=1 turbo run dev",
            "cross-env PORT=3000 bun run dev",
            "dotenv -e .env -- next dev",
            "NODE_EXTRA_CA_CERTS=./ca.pem dotenv -e .env -- next dev --experimental-https",
            "dotenv -e .env next dev",
            "bunx dotenv -- next dev",
            "cross-env-shell NODE_ENV=dev \"next dev\"",
            "cross-env-shell NODE_ENV=dev next dev",
            "dotenvx run -f .env -- next dev",
            "env-cmd -f .env next dev",
            "env-cmd -- next dev",
        ] {
            assert_eq!(kind(command), Some(Kind::DevServer), "{command}");
        }
        for command in [
            "cross-env NODE_ENV=production bun run build",
            "dotenv -e .env -- bun run build",
            "cross-env-shell NODE_ENV=production \"bun run build\"",
            "env-cmd -f .env bun run build",
        ] {
            assert_eq!(kind(command), None, "{command}");
        }
    }

    #[test]
    fn the_command_line_cross_env_shell_hands_to_a_shell_is_read_as_the_guard_reads_its_own() {
        let recognized = recognize("cross-env-shell NODE_ENV=dev \"cd apps/web && next dev\"").unwrap();
        assert_eq!(recognized.command, "next dev");
        assert_eq!(recognized.moved_to.as_deref(), Some("apps/web"));
    }

    #[test]
    fn a_command_inside_quotes_or_an_argument_starts_nothing() {
        for command in [
            "echo 'npm run dev'",
            "echo \"a && npm run dev\"",
            "git commit -m 'docker compose up'",
            "grep -rn 'next dev' src",
            "cat package.json | grep dev",
            "man docker-compose",
            "which vite",
            "command -v vite",
            "ls dev",
            "bash --norc script.sh",
            "vite --version",
            "curl http://localhost:3000/dev",
            "",
            "   ",
        ] {
            assert_eq!(kind(command), None, "{command}");
        }
    }

    #[test]
    fn the_recognized_command_is_the_one_that_matched_as_typed() {
        let recognized = recognize("cd apps/web && PORT=3000 npm run dev --silent").unwrap();
        assert_eq!(recognized.command, "PORT=3000 npm run dev --silent");
        let recognized = recognize("bash -c 'cd web; next dev'").unwrap();
        assert_eq!(recognized.command, "next dev");
    }

    #[test]
    fn only_a_bash_call_with_a_command_is_a_tool_call_to_judge() {
        let call = tool_call(r#"{"tool_name":"Bash","tool_input":{"command":"npm run dev"},"cwd":"/p/work/app"}"#);
        assert_eq!(
            call,
            Some(ToolCall {
                command: "npm run dev".to_owned(),
                cwd: Some(PathBuf::from("/p/work/app")),
            })
        );
        assert_eq!(
            tool_call(r#"{"tool_name":"Edit","tool_input":{"command":"npm run dev"}}"#),
            None
        );
        assert_eq!(tool_call(r#"{"tool_name":"Bash","tool_input":{}}"#), None);
        assert_eq!(tool_call("not json"), None);
        assert_eq!(tool_call(r#""npm run dev""#), None);
    }

    fn process(name: &str, run: &str) -> GuardService {
        GuardService {
            name: name.to_owned(),
            run: Some(run.to_owned()),
            compose: None,
        }
    }

    #[test]
    fn the_denial_names_the_command_the_services_and_the_procly_command() {
        let recognized = recognize("npm run dev").unwrap();
        let services = [process("api", "node api.js"), process("web", "next dev")];
        let answer: Value =
            serde_json::from_str(&denial(&recognized, &services, Some(Path::new("")), root(), None)).unwrap();
        let reason = answer["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .unwrap();
        assert!(reason.starts_with("`npm run dev` starts a dev server."), "{reason}");
        assert!(
            reason.contains("Run `procly up` instead.") && reason.ends_with("Declared Services: api, web."),
            "{reason}"
        );
        assert_eq!(answer["hookSpecificOutput"]["permissionDecision"], "deny");
    }

    #[test]
    fn the_equivalent_is_the_service_whose_command_starts_the_same_way() {
        let services = [
            process("web", "pnpm --filter web dev --port {ports.http}"),
            process("docs", "cd docs && npx astro dev"),
            process("dev", "pnpm dev"),
        ];
        let equivalent = |command: &str, directory: &str| {
            equivalent(
                &recognize(command).unwrap(),
                &services,
                Some(Path::new(directory)),
                root(),
                None,
            )
        };
        assert_eq!(
            equivalent("pnpm --filter web dev", "").as_deref(),
            Some("procly up web")
        );
        assert_eq!(
            equivalent("pnpm --filter web dev --port 3000", "").as_deref(),
            Some("procly up web")
        );
        assert_eq!(
            equivalent("cd docs && astro dev --host", "").as_deref(),
            Some("procly up docs")
        );
        assert_eq!(equivalent("astro dev --host", ""), None);
        assert_eq!(equivalent("pnpm dev", "").as_deref(), Some("procly up dev"));
        assert_eq!(equivalent("pnpm --filter api dev", ""), None);
    }

    #[test]
    fn the_equivalent_runs_from_the_same_directory_and_is_never_a_guess_between_equals() {
        let services = [
            process("api", "cd apps/api && pnpm dev"),
            process("web", "cd apps/web/ && pnpm dev"),
            process("admin", "pnpm dev --filter admin"),
            process("shop", "pnpm dev --filter shop"),
        ];
        let equivalent = |command: &str| {
            equivalent(
                &recognize(command).unwrap(),
                &services,
                Some(Path::new("")),
                root(),
                None,
            )
        };
        assert_eq!(equivalent("cd apps/api && pnpm dev").as_deref(), Some("procly up api"));
        assert_eq!(
            equivalent("cd ./apps/web && pnpm dev").as_deref(),
            Some("procly up web")
        );
        assert_eq!(equivalent("cd apps/docs && pnpm dev"), None);
        // `pnpm dev` starts both admin and shop as far as words go, neither is a better match.
        assert_eq!(equivalent("pnpm dev"), None);
    }

    #[test]
    fn a_dev_command_of_the_whole_repository_at_its_root_is_procly_up() {
        let services = [process("web", "node server.js")];
        let equivalent = |command: &str, directory: &str| {
            equivalent(
                &recognize(command).unwrap(),
                &services,
                Some(Path::new(directory)),
                root(),
                None,
            )
        };
        for command in [
            "npm start",
            "pnpm dev",
            "yarn run dev",
            "bun run serve",
            "turbo run dev",
        ] {
            assert_eq!(equivalent(command, "").as_deref(), Some("procly up"), "{command}");
            assert_eq!(equivalent(command, "apps/web"), None, "{command}");
        }
        for command in [
            "cd apps/web && pnpm dev",
            "pnpm --filter web dev",
            "npm run dev:web",
            "next dev",
        ] {
            assert_eq!(equivalent(command, ""), None, "{command}");
        }
    }

    #[test]
    fn an_option_written_with_its_value_after_an_equals_sign_narrows_the_command_to_one_package() {
        for command in [
            "turbo dev --filter=@acme/web",
            "npm run dev --workspace=web",
            "pnpm --filter=web dev",
            "bun --cwd=apps/web dev",
            "pnpm --dir=apps/web dev",
            "npm --prefix=apps/web start",
            "pnpm -C=apps/web dev",
        ] {
            assert!(!runs_the_whole_repository(&words_of(command)), "{command}");
        }
        assert!(runs_the_whole_repository(&words_of("turbo dev")));
        // Nothing declared starts it, so the agent is left to pick the Service, never sent to the whole Repository.
        let services = [process("dev", "bun run dev")];
        assert_eq!(
            equivalent(
                &recognize("turbo dev --filter=@acme/web").unwrap(),
                &services,
                Some(Path::new("")),
                root(),
                None
            ),
            None
        );
    }

    #[test]
    fn a_compose_command_is_the_compose_services_it_names_or_all_of_them() {
        let compose = |name: &str, service: &str| GuardService {
            name: name.to_owned(),
            run: None,
            compose: Some(service.to_owned()),
        };
        let services = [
            compose("cache", "redis"),
            compose("db", "postgres"),
            process("web", "next dev"),
        ];
        let equivalent = |command: &str| {
            equivalent(
                &recognize(command).unwrap(),
                &services,
                Some(Path::new("")),
                root(),
                None,
            )
        };
        assert_eq!(
            equivalent("docker compose -f compose.yaml up -d postgres").as_deref(),
            Some("procly up db")
        );
        assert_eq!(
            equivalent("docker-compose up --wait redis postgres").as_deref(),
            Some("procly up cache && procly up db")
        );
        assert_eq!(
            equivalent("docker compose up -d").as_deref(),
            Some("procly up cache && procly up db")
        );
        assert_eq!(equivalent("docker compose up mailpit"), None);
    }

    #[test]
    fn the_equivalent_is_the_service_that_runs_in_the_directory_the_command_runs_in() {
        let services = [
            process("web", "cd apps/web && bun run dev"),
            process("api", "cd apps/api && bun run dev"),
        ];
        let equivalent = |command: &str, directory: Option<&Path>| {
            equivalent(&recognize(command).unwrap(), &services, directory, root(), None)
        };
        // Run from the directory of the Service, or moved there by a `cd`, whatever the way it is written.
        assert_eq!(
            equivalent("bun dev", Some(Path::new("apps/web"))).as_deref(),
            Some("procly up web")
        );
        assert_eq!(
            equivalent("bun run dev", Some(Path::new("apps/api"))).as_deref(),
            Some("procly up api")
        );
        assert_eq!(
            equivalent("cd apps/web && bun dev", Some(Path::new(""))).as_deref(),
            Some("procly up web")
        );
        assert_eq!(
            equivalent("cd web && bun dev", Some(Path::new("apps"))).as_deref(),
            Some("procly up web")
        );
        assert_eq!(
            equivalent("cd ../api && bun dev", Some(Path::new("apps/web"))).as_deref(),
            Some("procly up api")
        );
        // A directory no Service runs in, the root when no Service runs the whole Repository there, and one that
        // is not known, name no Service.
        assert_eq!(equivalent("bun dev", Some(Path::new("apps/docs"))), None);
        assert_eq!(equivalent("bun dev", Some(Path::new("apps"))), None);
        assert_eq!(equivalent("cd apps/web && bun dev", None), None);
    }

    #[test]
    fn a_directory_option_moves_the_command_like_a_cd_and_is_not_compared_with_the_run() {
        let services = [
            process("web", "cd apps/web && bun run dev"),
            process("api", "cd apps/api && bun run dev"),
            process("dev", "bun run dev"),
            process("docs", "npm --prefix=apps/docs run dev"),
        ];
        let equivalent = |command: &str, directory: &str| {
            equivalent(
                &recognize(command).unwrap(),
                &services,
                Some(Path::new(directory)),
                root(),
                None,
            )
        };
        for (command, directory, instead) in [
            ("bun --cwd apps/web run dev", "", "procly up web"),
            ("bun run --cwd apps/web dev", "", "procly up web"),
            ("bun --cwd=apps/web dev", "", "procly up web"),
            ("bun --cwd web run dev", "apps", "procly up web"),
            ("cd apps && bun --cwd web dev", "", "procly up web"),
            ("bun --cwd ../api dev", "apps/web", "procly up api"),
            ("bun --cwd . run dev", "", "procly up dev"),
            // The directory option of a Service is where it runs too.
            ("cd apps/docs && npm run dev", "", "procly up docs"),
            ("npm --prefix apps/docs run dev", "", "procly up docs"),
        ] {
            assert_eq!(equivalent(command, directory).as_deref(), Some(instead), "{command}");
        }
        // Moved to the directory of web, but another program than the one its `run` names: the agent picks.
        assert_eq!(equivalent("yarn --cwd apps/web dev", ""), None);
        assert_eq!(equivalent("bun --cwd apps/other run dev", ""), None);
    }

    #[test]
    fn an_absolute_cd_into_the_worktree_is_placed_as_the_relative_one_and_one_outside_it_is_not() {
        let services = [
            process("web", "cd apps/web && bun run dev"),
            process("api", "cd apps/api && bun run dev"),
            process("dev", "bun run dev"),
        ];
        let equivalent = |command: &str, directory: Option<&Path>| {
            equivalent(&recognize(command).unwrap(), &services, directory, root(), None)
        };
        for (command, directory, instead) in [
            ("cd /p/work/acme/web/apps/web && bun run dev", Some(""), "procly up web"),
            (
                "cd /p/work/acme/web/apps/web/ && bun dev",
                Some("apps/api"),
                "procly up web",
            ),
            (
                "pushd /p/work/acme/web/apps/../apps/api && bun dev",
                Some(""),
                "procly up api",
            ),
            ("cd /p/work/acme/web && bun run dev", Some("apps/web"), "procly up dev"),
            ("bun --cwd /p/work/acme/web/apps/web run dev", Some(""), "procly up web"),
            // An absolute path does not need to know where the agent is.
            ("cd /p/work/acme/web/apps/web && bun dev", None, "procly up web"),
        ] {
            assert_eq!(
                equivalent(command, directory.map(Path::new)).as_deref(),
                Some(instead),
                "{command}"
            );
        }
        for command in [
            "cd /srv/app && bun dev",
            "cd /p/work/acme/web2/apps/web && bun dev",
            "cd /p/work/acme/web/../apps/web && bun dev",
        ] {
            assert_eq!(equivalent(command, Some(Path::new(""))), None, "{command}");
        }
    }

    #[test]
    fn the_directory_of_a_service_is_where_the_cd_at_the_head_of_its_run_leads_and_else_the_root() {
        for (run, directory) in [
            ("bun run dev", Some("")),
            ("cd apps/web && bun run dev", Some("apps/web")),
            ("cd ./apps/web/ && bun run dev", Some("apps/web")),
            ("cd apps && cd web; next dev", Some("apps/web")),
            ("pushd /p/work/acme/web/apps/api && bun dev", Some("apps/api")),
            // Only a `cd` at the head moves it.
            ("bun install && cd apps/web && bun dev", Some("")),
            ("cd /srv/app && bun dev", None),
            ("cd .. && bun dev", None),
            ("cd && bun dev", None),
            ("cd ~/app && bun dev", None),
        ] {
            assert_eq!(service_directory(run, root()), directory.map(PathBuf::from), "{run}");
        }
    }

    #[test]
    fn an_absolute_cd_through_a_symlink_is_placed_where_the_symlink_leads() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("worktree");
        std::fs::create_dir_all(root.join("apps/web")).unwrap();
        std::fs::create_dir_all(temporary.path().join("elsewhere/apps/web")).unwrap();
        std::os::unix::fs::symlink(&root, temporary.path().join("link")).unwrap();
        std::os::unix::fs::symlink(temporary.path().join("elsewhere"), temporary.path().join("away")).unwrap();
        let services = [process("web", "cd apps/web && bun run dev")];
        let equivalent = |command: String, root: &Path| {
            equivalent(
                &recognize(&command).unwrap(),
                &services,
                Some(Path::new("")),
                root,
                None,
            )
        };
        let link = temporary.path().join("link").display().to_string();
        let canonical = root.canonicalize().unwrap();
        // Through a symlink to the root, and to a root the Daemon gives canonical, as `/private/tmp` for `/tmp`.
        assert_eq!(
            equivalent(format!("cd {link}/apps/web && bun dev"), &root).as_deref(),
            Some("procly up web")
        );
        assert_eq!(
            equivalent(format!("cd {}/apps/web && bun dev", root.display()), &canonical).as_deref(),
            Some("procly up web")
        );
        // A symlink that leads out of the Worktree, and a directory that does not exist, are not inside it.
        let away = temporary.path().join("away").display().to_string();
        assert_eq!(equivalent(format!("cd {away}/apps/web && bun dev"), &root), None);
        assert_eq!(equivalent(format!("cd {link}/apps/gone && bun dev"), &root), None);
    }

    #[test]
    fn a_service_of_the_root_is_not_the_service_of_a_directory_below_it() {
        let services = [process("web", "bun run dev")];
        let equivalent = |directory: &str| {
            equivalent(
                &recognize("bun dev").unwrap(),
                &services,
                Some(Path::new(directory)),
                root(),
                None,
            )
        };
        assert_eq!(equivalent("").as_deref(), Some("procly up web"));
        assert_eq!(equivalent("apps/web"), None);
    }

    #[test]
    fn a_compose_command_names_the_compose_files_it_is_given() {
        let unmanaged = |command: &str, directory: &str| {
            unmanaged_compose_file(&recognize(command).unwrap(), Some(Path::new(directory)), root(), None)
                .map(|unmanaged| unmanaged.typed)
        };
        for (command, directory, file) in [
            ("docker compose -f dev.yml up", "", "dev.yml"),
            ("docker compose --file compose.dev.yaml up -d", "", "compose.dev.yaml"),
            ("docker compose --file=infra/compose.yaml up", "", "infra/compose.yaml"),
            ("docker compose -fdev.yml up", "", "dev.yml"),
            ("docker compose -f=dev.yml up", "", "dev.yml"),
            (
                "docker-compose -f compose.yaml -f override.yml up -d",
                "",
                "override.yml",
            ),
            ("docker compose -f compose.yaml up", "apps/web", "compose.yaml"),
            (
                "docker compose -f ../../../compose.yaml up",
                "apps/web",
                "../../../compose.yaml",
            ),
            ("cd infra && docker compose -f compose.yaml up", "", "compose.yaml"),
        ] {
            assert_eq!(unmanaged(command, directory).as_deref(), Some(file), "{command}");
        }
    }

    #[test]
    fn the_compose_file_of_the_root_and_a_command_that_names_none_are_managed() {
        let unmanaged = |command: &str, directory: &str| {
            unmanaged_compose_file(&recognize(command).unwrap(), Some(Path::new(directory)), root(), None)
        };
        for (command, directory) in [
            ("docker compose up", ""),
            ("docker compose up -f", ""),
            ("docker compose -f compose.yaml up", ""),
            ("docker compose -f ./docker-compose.yml up", ""),
            ("docker compose --file=compose.yml up", ""),
            ("docker compose -f ../../compose.yaml up", "apps/web"),
            ("cd .. && docker compose -f compose.yaml up", "apps"),
            ("docker compose -p x --profile dev up db", ""),
            // The compose file of the root, named by its absolute path, from wherever the command runs.
            ("docker compose -f /p/work/acme/web/compose.yaml up", ""),
            (
                "docker compose --file=/p/work/acme/web/docker-compose.yml up",
                "apps/web",
            ),
            ("docker compose -f /p/work/acme/web/apps/../compose.yml up", ""),
            ("docker compose -f /p/work/acme/other/../web/compose.yaml up", ""),
            (
                "cd /elsewhere && docker compose -f /p/work/acme/web/compose.yaml up",
                "",
            ),
        ] {
            assert_eq!(unmanaged(command, directory), None, "{command}");
        }
        assert_eq!(
            unmanaged_compose_file(&recognize("docker compose -f dev.yml up").unwrap(), None, root(), None),
            None
        );
    }

    #[test]
    fn an_absolute_compose_file_other_than_the_one_of_the_root_is_not_managed() {
        let unmanaged = |command: &str, directory: &str| {
            unmanaged_compose_file(&recognize(command).unwrap(), Some(Path::new(directory)), root(), None)
                .map(|unmanaged| unmanaged.typed)
        };
        for (command, directory, file) in [
            // Inside the Worktree, a file Procly does not manage.
            (
                "docker compose -f /p/work/acme/web/infra/other.yml up -d",
                "",
                "/p/work/acme/web/infra/other.yml",
            ),
            (
                "docker compose -f /p/work/acme/web/infra/compose.yaml up -d",
                "apps/web",
                "/p/work/acme/web/infra/compose.yaml",
            ),
            (
                "docker compose -f /p/work/acme/web/../web/dev.yml up",
                "",
                "/p/work/acme/web/../web/dev.yml",
            ),
            // Outside it, even when it is named like the compose file of the root.
            (
                "docker compose -f /srv/app/compose.yaml up",
                "",
                "/srv/app/compose.yaml",
            ),
            (
                "docker compose -f /p/work/acme/web2/compose.yaml up",
                "",
                "/p/work/acme/web2/compose.yaml",
            ),
            (
                "docker compose -f /p/work/acme/web/../compose.yaml up",
                "",
                "/p/work/acme/web/../compose.yaml",
            ),
            (
                "docker compose -f /p/work/acme/web/compose.yaml -f /srv/override.yml up",
                "",
                "/srv/override.yml",
            ),
            // Where the command runs does not matter to an absolute path.
            (
                "cd /elsewhere && docker compose -f /srv/app/compose.yaml up",
                "",
                "/srv/app/compose.yaml",
            ),
        ] {
            assert_eq!(unmanaged(command, directory).as_deref(), Some(file), "{command}");
        }
        // The Daemon could not place the directory, an absolute file does not need it.
        assert_eq!(
            unmanaged_compose_file(
                &recognize("docker compose -f /p/work/acme/web/infra/other.yml up").unwrap(),
                None,
                root(),
                None
            )
            .map(|unmanaged| unmanaged.typed)
            .as_deref(),
            Some("/p/work/acme/web/infra/other.yml")
        );
    }

    #[test]
    fn a_path_above_the_root_of_the_worktree_keeps_its_parent_directories_and_matches_nothing_inside() {
        assert_eq!(
            resolve(Path::new("apps/web"), "../api"),
            Some(PathBuf::from("apps/api"))
        );
        assert_eq!(resolve(Path::new(""), ".."), Some(PathBuf::from("..")));
        assert_eq!(resolve(Path::new("apps"), "../../x"), Some(PathBuf::from("../x")));
        assert_eq!(resolve(Path::new("apps"), "/"), None);
        assert_eq!(resolve(Path::new("apps"), "/srv/app"), None);
        assert_eq!(resolve(Path::new("apps"), "~"), None);
        assert_eq!(resolve(Path::new("apps"), "~/web"), None);
    }

    #[test]
    fn an_absolute_path_is_relative_to_the_root_only_when_it_is_inside_it() {
        assert_eq!(
            within(root(), "/p/work/acme/web/apps/../compose.yml"),
            Path::new("compose.yml")
        );
        assert_eq!(within(root(), "/p/work/acme/web"), Path::new(""));
        assert_eq!(
            within(root(), "/p/work/acme/web/../compose.yml"),
            Path::new("/p/work/acme/compose.yml")
        );
        assert_eq!(
            within(root(), "/p/work/acme/web2/compose.yml"),
            Path::new("/p/work/acme/web2/compose.yml")
        );
        assert_eq!(within(root(), "/../srv"), Path::new("/srv"));
    }

    #[test]
    fn the_denial_of_a_compose_file_procly_does_not_manage_says_so_and_names_no_service() {
        let services = [GuardService {
            name: "db".to_owned(),
            run: None,
            compose: Some("postgres".to_owned()),
        }];
        let recognized = recognize("docker compose -f dev.yml up -d").unwrap();
        let answer: Value =
            serde_json::from_str(&denial(&recognized, &services, Some(Path::new("")), root(), None)).unwrap();
        let reason = answer["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .unwrap();
        assert!(
            reason.contains("`dev.yml` is not managed by Procly") && !reason.contains("procly up"),
            "{reason}"
        );
        assert_eq!(answer["hookSpecificOutput"]["permissionDecision"], "deny");
    }

    #[test]
    fn the_denial_of_an_absolute_compose_file_procly_does_not_manage_is_not_the_answer_of_the_root() {
        let services = [GuardService {
            name: "db".to_owned(),
            run: None,
            compose: Some("postgres".to_owned()),
        }];
        let reason = |command: &str| {
            let answer: Value = serde_json::from_str(&denial(
                &recognize(command).unwrap(),
                &services,
                Some(Path::new("")),
                root(),
                None,
            ))
            .unwrap();
            answer["hookSpecificOutput"]["permissionDecisionReason"]
                .as_str()
                .unwrap()
                .to_owned()
        };
        let other = reason("docker compose -f /p/work/acme/web/infra/other.yml up -d");
        assert!(
            other.contains("`/p/work/acme/web/infra/other.yml` is not managed by Procly")
                && !other.contains("procly up")
                && !other.contains("Declared Services"),
            "{other}"
        );
        let outside = reason("docker compose -f /srv/app/compose.yaml up -d");
        assert!(
            outside.contains("is not managed by Procly") && !outside.contains("procly up"),
            "{outside}"
        );
        let managed = reason("docker compose -f /p/work/acme/web/compose.yaml up -d");
        assert!(managed.contains("Run `procly up db` instead."), "{managed}");
    }

    /// A Worktree with a compose file in `apps/api` and none at its root, and the Compose services declared on it.
    fn subdirectory_worktree() -> (tempfile::TempDir, [GuardService; 2]) {
        let worktree = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(worktree.path().join("apps/api/src")).unwrap();
        std::fs::write(worktree.path().join("apps/api/docker-compose.yml"), "services: {}\n").unwrap();
        let compose = |name: &str, compose: &str| GuardService {
            name: name.to_owned(),
            run: None,
            compose: Some(compose.to_owned()),
        };
        (worktree, [compose("db", "postgres"), compose("cache", "redis")])
    }

    fn reason_of(answer: &str) -> String {
        let answer: Value = serde_json::from_str(answer).unwrap();
        assert_eq!(answer["hookSpecificOutput"]["permissionDecision"], "deny");
        answer["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    #[test]
    fn a_compose_command_on_the_declared_compose_file_is_the_procly_up_of_its_compose_services() {
        let (worktree, services) = subdirectory_worktree();
        let root = worktree.path();
        let declared = Some(Path::new("apps/api/docker-compose.yml"));
        let reason = |command: &str, directory: &str| {
            reason_of(&denial(
                &recognize(command).unwrap(),
                &services,
                Some(Path::new(directory)),
                root,
                declared,
            ))
        };
        for (command, directory) in [
            ("docker compose -f apps/api/docker-compose.yml up -d", ""),
            ("docker compose --file ./apps/api/docker-compose.yml up -d", ""),
            ("docker compose -f docker-compose.yml up -d", "apps/api"),
            ("cd apps/api && docker compose up -d", ""),
            ("docker compose up -d", "apps/api"),
            // Docker Compose looks for the compose file in the directories above the one it runs in.
            ("cd apps/api/src && docker compose up -d", ""),
            // With none to find, the command runs the compose file of no other place.
            ("docker compose up -d", ""),
        ] {
            let reason = reason(command, directory);
            assert!(
                reason.contains("Run `procly up db && procly up cache` instead.")
                    && reason.ends_with("Declared Services: db, cache."),
                "{command} in {directory:?}: {reason}"
            );
        }
        let named = reason("cd apps/api && docker compose up -d redis", "");
        assert!(named.contains("Run `procly up cache` instead."), "{named}");
        // A file at the root is no longer the one Procly runs once another is declared.
        std::fs::write(root.join("compose.yaml"), "services: {}\n").unwrap();
        for (command, typed) in [
            ("docker compose up -d", "compose.yaml"),
            ("docker compose -f compose.yaml up -d", "compose.yaml"),
        ] {
            let reason = reason(command, "");
            assert!(
                reason.contains(&format!(
                    "The compose file `{typed}` is not managed by Procly, which runs `apps/api/docker-compose.yml`"
                )) && !reason.contains("procly up"),
                "{command}: {reason}"
            );
        }
    }

    #[test]
    fn a_compose_file_outside_the_root_that_none_declares_is_judged_where_it_is_and_the_setting_is_named() {
        let (worktree, services) = subdirectory_worktree();
        let root = worktree.path();
        let reason = |command: &str| {
            reason_of(&denial(
                &recognize(command).unwrap(),
                &services,
                Some(Path::new("")),
                root,
                None,
            ))
        };
        for command in [
            "cd apps/api && docker compose up -d",
            "docker compose -f apps/api/docker-compose.yml up -d",
            "cd apps/api/src && docker compose up -d",
        ] {
            let reason = reason(command);
            assert!(
                reason.contains("is not managed by Procly")
                    && reason.contains(
                        "Declare it with `compose_file = \"apps/api/docker-compose.yml\"` in the [settings] of procly.toml"
                    )
                    && !reason.contains("Declared Services"),
                "{command}: {reason}"
            );
        }
        // Outside the Worktree, no setting declares it.
        let outside = reason("docker compose -f /srv/app/compose.yaml up -d");
        assert!(
            outside.contains("`/srv/app/compose.yaml` is not managed by Procly")
                && !outside.contains("compose_file = "),
            "{outside}"
        );
        // A compose file at the root is the one the command runs, wherever the command runs below it.
        std::fs::write(root.join("compose.yaml"), "services: {}\n").unwrap();
        std::fs::remove_file(root.join("apps/api/docker-compose.yml")).unwrap();
        let rooted = reason("cd apps/api && docker compose up -d");
        assert!(
            rooted.contains("Run `procly up db && procly up cache` instead."),
            "{rooted}"
        );
    }
}
