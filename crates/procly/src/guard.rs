//! The Guard, as the Claude Code hook `procly guard` runs it: which Bash commands start a dev server or containers
//! that Procly runs instead (see docs/agents.md for the list).
//!
//! The recognition happens in the client and not in the Daemon, since the hook runs before every Bash command of the
//! agent and a command the Guard does not know must neither wait for the Daemon nor start it. Only a command it
//! knows asks the Daemon whether the Repository is Off.

use std::path::PathBuf;

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
    /// Whether a `cd` or a `pushd` before it moved away from the directory the command line was run in.
    pub(crate) moved: bool,
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
/// run at the root of its Worktree, or else `procly up <service>` for the agent to pick among `services`. `at_root`
/// tells whether the command line was run at the root of the Worktree.
pub(crate) fn denial(recognized: &Recognized, services: &[GuardService], at_root: bool) -> String {
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
    let instead = match (equivalent(recognized, services, at_root), kind) {
        (Some(equivalent), _) => format!("Run `{equivalent}` instead."),
        (None, Kind::DevServer) => "Run `procly up <service>` instead.".to_owned(),
        (None, Kind::Compose) => "Run `procly up <service>` for the Compose service instead.".to_owned(),
    };
    let declared = if services.is_empty() {
        "No Service is declared in procly.toml yet, declare it there first.".to_owned()
    } else {
        let names: Vec<&str> = services.iter().map(|service| service.name.as_str()).collect();
        format!("Declared Services: {}.", names.join(", "))
    };
    json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": format!("`{command}` {why} {instead} {declared}"),
        }
    })
    .to_string()
}

/// The Procly command that does what the blocked command does, `None` when nothing declared tells.
fn equivalent(recognized: &Recognized, services: &[GuardService], at_root: bool) -> Option<String> {
    let blocked = words_of(&recognized.command);
    let blocked = strip_launchers(&blocked);
    match recognized.kind {
        Kind::DevServer => {
            let declared = services
                .iter()
                .filter_map(|service| {
                    let run = words_of_dev_command(service.run.as_deref()?);
                    let matched = same_start(blocked, strip_launchers(&run))?;
                    Some((matched, service))
                })
                // The longest match wins, the first declared one among equals.
                .fold(None::<(usize, &GuardService)>, |best, (matched, service)| match best {
                    Some((longest, _)) if longest >= matched => best,
                    _ => Some((matched, service)),
                });
            match declared {
                Some((_, service)) => Some(format!("procly up {}", service.name)),
                None => {
                    (at_root && !recognized.moved && runs_the_whole_repository(blocked)).then(|| "procly up".to_owned())
                }
            }
        }
        Kind::Compose => {
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

/// The words of the first simple command of `command`.
fn words_of(command: &str) -> Vec<String> {
    segments(command)
        .into_iter()
        .next()
        .map(|segment| segment.words)
        .unwrap_or_default()
}

/// The words of the command of a Service that starts its dev server, the first one when none does.
fn words_of_dev_command(run: &str) -> Vec<String> {
    words_of(&recognize(run).map_or_else(|| run.to_owned(), |recognized| recognized.command))
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

/// The services of the compose file `docker compose up` names, empty when it names none and starts them all.
fn compose_services(words: &[String]) -> Vec<&str> {
    let after = match words.first().map(|program| basename(program)) {
        Some("docker") => words
            .iter()
            .position(|word| word == "compose")
            .map_or(&[][..], |at| &words[at + 1..]),
        _ => words.get(1..).unwrap_or_default(),
    };
    positional(after, &COMPOSE_OPTIONS_WITH_VALUE)
        .into_iter()
        .skip_while(|word| !matches!(*word, "up" | "start"))
        .skip(1)
        .collect()
}

/// The first command of `command_line` that starts a dev server or containers, `None` when none does.
pub(crate) fn recognize(command_line: &str) -> Option<Recognized> {
    let mut moved = false;
    segments(command_line).into_iter().find_map(|segment| {
        let words = strip_launchers(&segment.words);
        if matches!(words.first().map(|word| basename(word)), Some("cd" | "pushd")) {
            moved = true;
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
                moved: moved || recognized.moved,
                ..recognized
            });
        }
        classify(words).map(|kind| Recognized {
            kind,
            command: segment.text.clone(),
            moved,
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
        let answer: Value = serde_json::from_str(&denial(&recognized, &services, true)).unwrap();
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
        let equivalent = |command: &str, at_root: bool| equivalent(&recognize(command).unwrap(), &services, at_root);
        assert_eq!(
            equivalent("pnpm --filter web dev", false).as_deref(),
            Some("procly up web")
        );
        assert_eq!(
            equivalent("pnpm --filter web dev --port 3000", false).as_deref(),
            Some("procly up web")
        );
        assert_eq!(equivalent("astro dev --host", true).as_deref(), Some("procly up docs"));
        assert_eq!(equivalent("pnpm dev", true).as_deref(), Some("procly up dev"));
        assert_eq!(equivalent("pnpm --filter api dev", true), None);
    }

    #[test]
    fn a_dev_command_of_the_whole_repository_at_its_root_is_procly_up() {
        let services = [process("web", "node server.js")];
        let equivalent = |command: &str, at_root: bool| equivalent(&recognize(command).unwrap(), &services, at_root);
        for command in [
            "npm start",
            "pnpm dev",
            "yarn run dev",
            "bun run serve",
            "turbo run dev",
        ] {
            assert_eq!(equivalent(command, true).as_deref(), Some("procly up"), "{command}");
            assert_eq!(equivalent(command, false), None, "{command}");
        }
        for command in [
            "cd apps/web && pnpm dev",
            "pnpm --filter web dev",
            "npm run dev:web",
            "next dev",
        ] {
            assert_eq!(equivalent(command, true), None, "{command}");
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
        let equivalent = |command: &str| equivalent(&recognize(command).unwrap(), &services, true);
        assert_eq!(
            equivalent("docker compose -f dev.yml up -d postgres").as_deref(),
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
}
