//! The Guard, as the Claude Code hook `procly guard` runs it: which Bash commands start a dev server or containers
//! that Procly runs instead (see docs/agents.md for the list).
//!
//! The recognition happens in the client and not in the Daemon, since the hook runs before every Bash command of the
//! agent and a command the Guard does not know must neither wait for the Daemon nor start it. Only a command it
//! knows asks the Daemon whether the Repository is Off.

use std::path::{Component, Path, PathBuf};

use serde_json::{Value, json};

use crate::protocol::GuardService;

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
pub(crate) fn denial(
    recognized: &Recognized,
    services: &[GuardService],
    directory: Option<&Path>,
    root: &Path,
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
    let instead = match (equivalent(recognized, services, directory, root), kind) {
        (Some(equivalent), _) => format!("Run `{equivalent}` instead."),
        (None, Kind::Compose) if let Some(file) = unmanaged_compose_file(recognized, directory, root) => {
            // Nothing else to say: the Services of the root compose file would read as a way to start this one.
            let reason = format!(
                "The compose file `{file}` is not managed by Procly, which runs the compose file at the root of the \
                 Worktree only, so no Procly command starts it."
            );
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
/// the command line was run, relative to the root of the Worktree, and `root` is the root itself.
fn equivalent(
    recognized: &Recognized,
    services: &[GuardService],
    directory: Option<&Path>,
    root: &Path,
) -> Option<String> {
    let blocked = words_of(&recognized.command);
    let blocked = strip_launchers(&blocked);
    match recognized.kind {
        Kind::DevServer => {
            // Run from the same directory, so that `cd apps/api && pnpm dev` is never the `pnpm dev` of `apps/web`,
            // and `pnpm dev` run in `apps/web` is the Service that changes to it.
            let place = place_of(recognized, directory);
            let matches: Vec<(usize, &GuardService)> = services
                .iter()
                .filter_map(|service| {
                    let run = recognize(service.run.as_deref()?)?;
                    if place.is_none() || place != resolve(Path::new(""), run.moved_to.as_deref().unwrap_or(".")) {
                        return None;
                    }
                    let matched = same_start(
                        &without_run(blocked),
                        &without_run(strip_launchers(&words_of(&run.command))),
                    )?;
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
            if unmanaged_compose_file(recognized, directory, root).is_some() {
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

/// The compose file a compose command is given that Procly does not manage, as typed, `None` when every file it is
/// given is the compose file at the root of the Worktree, when it is given none, and when a file cannot be placed
/// from where it is written. Procly drives that one file only (ADR 0004), so no Service of `procly.toml` is declared on
/// another. A file given with an absolute path is placed against `root`, the root of the Worktree: the compose file of
/// the root named that way is managed, any other absolute path, inside the Worktree or not, is not.
fn unmanaged_compose_file(recognized: &Recognized, directory: Option<&Path>, root: &Path) -> Option<String> {
    let place = place_of(recognized, directory);
    let blocked = words_of(&recognized.command);
    compose_files(strip_launchers(&blocked))
        .into_iter()
        .find(|file| {
            let resolved = if file.starts_with('/') {
                Some(within(root, file))
            } else {
                place.as_deref().and_then(|place| resolve(place, file))
            };
            resolved.is_some_and(|resolved| !COMPOSE_FILES.iter().any(|name| resolved == Path::new(name)))
        })
        .map(str::to_owned)
}

/// The names Docker Compose looks for, which Procly drives when the Worktree has one.
const COMPOSE_FILES: [&str; 4] = [
    "compose.yaml",
    "compose.yml",
    "docker-compose.yaml",
    "docker-compose.yml",
];

/// Where the recognized command runs, relative to the root of the Worktree: `directory` moved by the `cd` before it.
/// `None` when the directory is not known and when the `cd` leaves it to a place only an absolute path tells.
fn place_of(recognized: &Recognized, directory: Option<&Path>) -> Option<PathBuf> {
    resolve(directory?, recognized.moved_to.as_deref().unwrap_or("."))
}

/// `path` as written from `base`, a directory relative to the root of the Worktree, with its `.` and `..` resolved. A
/// path that climbs above the root keeps its leading `..`, so that it matches nothing inside. `None` for a path that
/// starts at `/` or at `~`, which says nothing about where the Worktree is.
fn resolve(base: &Path, path: &str) -> Option<PathBuf> {
    if path.starts_with('~') {
        return None;
    }
    let mut resolved = PathBuf::new();
    for component in base.join(path).components() {
        match component {
            Component::Normal(name) => resolved.push(name),
            Component::CurDir => {}
            Component::ParentDir if resolved.file_name().is_some_and(|name| name != "..") => {
                resolved.pop();
            }
            Component::ParentDir => resolved.push(".."),
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(resolved)
}

/// The absolute `path`, with its `.` and `..` resolved, relative to `root` when it is inside it. A path outside `root`
/// stays absolute, so that it matches nothing inside.
fn within(root: &Path, path: &str) -> PathBuf {
    let mut absolute = PathBuf::new();
    for component in Path::new(path).components() {
        match component {
            Component::ParentDir => {
                absolute.pop();
            }
            Component::CurDir => {}
            other => absolute.push(other),
        }
    }
    absolute.strip_prefix(root).map_or(absolute.clone(), Path::to_owned)
}

/// `words` without the `run` of a package manager, which `bun run dev` and `bun dev` both run the script `dev` with.
fn without_run(words: &[String]) -> Vec<String> {
    let mut words = words.to_vec();
    if matches!(
        words.first().map(|program| basename(program)),
        Some("npm" | "pnpm" | "yarn" | "bun")
    ) && words.get(1).is_some_and(|word| word == "run")
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

/// The package managers whose dev script, run at the root of a Repository, runs the dev servers of all of it.
const ROOT_RUNNERS: [&str; 5] = ["npm", "pnpm", "yarn", "bun", "turbo"];

/// The options of a package manager that narrow a command to one package of a monorepo.
const PACKAGE_OPTIONS: [&str; 8] = [
    "--workspace",
    "-w",
    "--prefix",
    "--filter",
    "-F",
    "--dir",
    "-C",
    "--cwd",
];

/// Whether `words` run the dev script of a whole Repository: `pnpm dev`, `npm start`, `turbo run dev`, with no
/// option narrowing it to one package and a plain script name.
fn runs_the_whole_repository(words: &[String]) -> bool {
    let Some((program, rest)) = words.split_first() else {
        return false;
    };
    if !ROOT_RUNNERS.contains(&basename(program)) || rest.iter().any(|word| PACKAGE_OPTIONS.contains(&word.as_str())) {
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
        // A shell given a command line to run: `bash -c 'npm run dev'`.
        if let [shell, flag, inner, ..] = words
            && matches!(basename(shell), "sh" | "bash" | "zsh" | "dash")
            && flag.starts_with('-')
            && !flag.starts_with("--")
            && flag.contains('c')
        {
            return recognize(inner).map(|recognized| Recognized {
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

/// A simple command of a command line: its text as typed, and its words with quotes and escapes resolved.
#[derive(Debug)]
struct Segment {
    text: String,
    words: Vec<String>,
}

/// Splits a command line into simple commands at semicolons, `&`, `|`, parentheses, backticks and line breaks, and each
/// command into words, the way a shell would outside of quotes. A separator inside quotes splits nothing, so
/// `echo "a && npm run dev"` is one command that starts nothing.
fn segments(command_line: &str) -> Vec<Segment> {
    #[derive(Default)]
    struct Reading {
        segments: Vec<Segment>,
        words: Vec<String>,
        word: Option<String>,
        start: usize,
    }
    impl Reading {
        /// The word being read, started if there is none.
        fn current(&mut self) -> &mut String {
            self.word.get_or_insert_with(String::new)
        }
        fn end_word(&mut self) {
            if let Some(word) = self.word.take() {
                self.words.push(word);
            }
        }
        fn end_segment(&mut self, end: usize, text: &str) {
            self.end_word();
            if !self.words.is_empty() {
                self.segments.push(Segment {
                    text: text[self.start..end].trim().to_owned(),
                    words: std::mem::take(&mut self.words),
                });
            }
        }
    }

    let mut reading = Reading::default();
    let mut quote: Option<char> = None;
    let mut chars = command_line.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        match (quote, c) {
            (Some(open), c) if c == open => quote = None,
            (Some('"'), '\\') => {
                if let Some((_, escaped)) = chars.next() {
                    reading.current().push(escaped);
                }
            }
            (None, '\'' | '"') => {
                quote = Some(c);
                reading.current();
            }
            (None, '\\') => match chars.next() {
                Some((_, '\n')) | None => {}
                Some((_, escaped)) => reading.current().push(escaped),
            },
            (None, ' ' | '\t') => reading.end_word(),
            (None, '\n' | ';' | '&' | '|' | '(' | ')' | '`') => {
                reading.end_segment(at, command_line);
                reading.start = at + c.len_utf8();
            }
            (_, c) => reading.current().push(c),
        }
    }
    reading.end_segment(command_line.len(), command_line);
    reading.segments
}

/// The last component of a program path: `./node_modules/.bin/next` is `next`.
fn basename(program: &str) -> &str {
    program.rsplit('/').next().unwrap_or(program)
}

/// Whether a word is a `NAME=value` assignment, which a shell applies to the command that follows it.
fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        name.chars()
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// The words of a command without what only launches it: variables set for it, `nohup`, `exec`, `time`, `npx`,
/// `pnpm exec`, `bundle exec` and the like.
fn strip_launchers(mut words: &[String]) -> &[String] {
    loop {
        let Some(first) = words.first() else {
            return words;
        };
        let second = words.get(1).map(String::as_str);
        let skip = match (basename(first), second) {
            _ if is_assignment(first) => 1,
            // `command -v vite` asks where a program is and starts nothing.
            ("command", Some("-v" | "-V")) => return words,
            // Also the keywords that open a command in a shell script: `if ready; then npm run dev; fi`.
            (
                "env" | "nohup" | "exec" | "time" | "command" | "setsid" | "npx" | "bunx" | "pnpx" | "then" | "do"
                | "else" | "!" | "{",
                _,
            ) => 1,
            ("pnpm" | "yarn", Some("exec" | "dlx"))
            | ("bun", Some("x"))
            | ("npm" | "bundle", Some("exec"))
            | ("poetry" | "uv", Some("run")) => 2,
            _ => return words,
        };
        words = &words[skip..];
        // The options of the wrapper, such as `npx -y`.
        while words.first().is_some_and(|word| word.starts_with('-')) {
            words = &words[1..];
        }
    }
}

/// The words after the program that are not options, skipping the value of an option that takes one.
fn positional<'a>(words: &'a [String], with_value: &[&str]) -> Vec<&'a str> {
    let mut positional = Vec::new();
    let mut words = words.iter().map(String::as_str);
    while let Some(word) = words.next() {
        if with_value.contains(&word) {
            words.next();
        } else if !word.starts_with('-') {
            positional.push(word);
        }
    }
    positional
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
            let positional = positional(rest, &["--workspace", "--prefix", "--filter", "-F", "--dir", "-C"]);
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
        let answer: Value = serde_json::from_str(&denial(&recognized, &services, Some(Path::new("")), root())).unwrap();
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
        let equivalent =
            |command: &str| equivalent(&recognize(command).unwrap(), &services, Some(Path::new("")), root());
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
        let equivalent =
            |command: &str| equivalent(&recognize(command).unwrap(), &services, Some(Path::new("")), root());
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
            equivalent(&recognize(command).unwrap(), &services, directory, root())
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
    fn a_service_of_the_root_is_not_the_service_of_a_directory_below_it() {
        let services = [process("web", "bun run dev")];
        let equivalent = |directory: &str| {
            equivalent(
                &recognize("bun dev").unwrap(),
                &services,
                Some(Path::new(directory)),
                root(),
            )
        };
        assert_eq!(equivalent("").as_deref(), Some("procly up web"));
        assert_eq!(equivalent("apps/web"), None);
    }

    #[test]
    fn a_compose_command_names_the_compose_files_it_is_given() {
        let unmanaged = |command: &str, directory: &str| {
            unmanaged_compose_file(&recognize(command).unwrap(), Some(Path::new(directory)), root())
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
            unmanaged_compose_file(&recognize(command).unwrap(), Some(Path::new(directory)), root())
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
            unmanaged_compose_file(&recognize("docker compose -f dev.yml up").unwrap(), None, root()),
            None
        );
    }

    #[test]
    fn an_absolute_compose_file_other_than_the_one_of_the_root_is_not_managed() {
        let unmanaged = |command: &str, directory: &str| {
            unmanaged_compose_file(&recognize(command).unwrap(), Some(Path::new(directory)), root())
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
                root()
            )
            .as_deref(),
            Some("/p/work/acme/web/infra/other.yml")
        );
    }

    #[test]
    fn the_denial_of_a_compose_file_procly_does_not_manage_says_so_and_names_no_service() {
        let services = [GuardService {
            name: "db".to_owned(),
            run: None,
            compose: Some("postgres".to_owned()),
        }];
        let recognized = recognize("docker compose -f dev.yml up -d").unwrap();
        let answer: Value = serde_json::from_str(&denial(&recognized, &services, Some(Path::new("")), root())).unwrap();
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
}
