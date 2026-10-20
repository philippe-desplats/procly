//! The command a Group runs once the commands of the root `dev` script that init did not recognise are left out.
//!
//! A root `dev` script that is one command, such as `turbo dev` or `pnpm -r --parallel dev`, is never judged: the
//! Group runs it through the package manager. A script that lists commands, with `&`, `&&`, `;` or
//! as the arguments of `concurrently`, is read command by command. A command is recognised when the Guard knows it as
//! a dev server, and the Group runs the recognised ones, written as the script wrote them.

use std::ops::Range;

use super::scripts::shell_word;
use crate::guard::{self, Kind};
use crate::shell::{self, Segment};

/// What the root `dev` script becomes as the command of a Group.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Narrowed {
    /// The script is one command, or every command of it is recognised: the Group runs the script as it is.
    Whole,
    /// The Group runs `run`, which starts `commands`, and `notes` name the commands it left out.
    Kept {
        run: String,
        commands: Vec<Command>,
        notes: Vec<String>,
    },
    /// No command is recognised, so there is no Group to propose.
    Nothing { notes: Vec<String> },
}

/// A command the Group runs that starts a dev server, with the directories the script changed to before it.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Command {
    pub(super) text: String,
    /// The arguments of the `cd` commands that ran before it, in order.
    pub(super) directories: Vec<String>,
}

/// The options of `concurrently` that are followed by their value.
const CONCURRENTLY_OPTIONS_WITH_VALUE: [&str; 22] = [
    "-n",
    "--names",
    "-c",
    "--prefix-colors",
    "-p",
    "--prefix",
    "-l",
    "--prefix-length",
    "-t",
    "--timestamp-format",
    "-s",
    "--success",
    "-m",
    "--max-processes",
    "--restart-tries",
    "--restart-after",
    "--kill-signal",
    "--kill-timeout",
    "--teardown",
    "--hide",
    "--default-input-target",
    "--name-separator",
];

/// The options of `concurrently` that give a comma-separated list with one entry per command.
const CONCURRENTLY_LISTS: [(&str, &str); 2] = [("-n", "--names"), ("-c", "--prefix-colors")];

/// One command `concurrently` runs, an argument of it.
#[derive(Debug)]
struct Child {
    /// The index of the argument among the words of its step.
    index: usize,
    /// The command as the argument holds it.
    text: String,
    /// Whether the Guard knows the command as a dev server.
    recognised: bool,
}

/// What a command of the script is for the Group.
#[derive(Debug)]
enum Role {
    /// A command that only sets the scene for the next ones: `cd apps/web`, `export NODE_ENV=development`.
    Context,
    /// A command that starts a dev server.
    DevServer,
    /// A command that starts something else.
    Other,
    /// A `wait`, which stays as long as a command before it is run.
    Wait,
    /// `concurrently` and the commands it runs.
    Concurrently(Vec<Child>),
    /// `concurrently` with options this reading does not know, which the Group runs as it is.
    Opaque,
}

/// One command of the script, as the Group sees it.
#[derive(Debug)]
struct Step<'a> {
    segment: &'a Segment,
    role: Role,
}

impl Step<'_> {
    /// Whether the Group runs the step, its context aside.
    fn recognised(&self) -> bool {
        match &self.role {
            Role::DevServer | Role::Opaque => true,
            Role::Context | Role::Wait | Role::Other => false,
            Role::Concurrently(children) => children.iter().any(|child| child.recognised),
        }
    }

    /// Whether the step leaves out something the script starts.
    fn leaves_out(&self) -> bool {
        match &self.role {
            Role::Other => true,
            Role::Concurrently(children) => children.iter().any(|child| !child.recognised),
            Role::Context | Role::Wait | Role::DevServer | Role::Opaque => false,
        }
    }
}

/// Whether the Guard knows every command of `command` as a dev server, the ones that only set the scene aside. A
/// command such as `stripe listen & next dev` starts something else too, so it is not.
fn starts_a_dev_server(command: &str) -> bool {
    let Some((segments, _)) = split(command) else {
        return guard::recognize(command).is_some_and(|recognized| recognized.kind == Kind::DevServer);
    };
    let mut dev_servers = 0;
    for segment in segments {
        if sets_the_scene(&segment) {
            continue;
        }
        if !guard::recognize(&segment.text).is_some_and(|recognized| recognized.kind == Kind::DevServer) {
            return false;
        }
        dev_servers += 1;
    }
    dev_servers > 0
}

/// Whether `segment` only sets the scene for the commands after it: `cd apps/web`, `export NODE_ENV=development`.
pub(super) fn sets_the_scene(segment: &Segment) -> bool {
    let words = &segment.words;
    words.iter().all(|word| shell::is_assignment(word))
        || words
            .first()
            .is_some_and(|word| matches!(shell::basename(word), "cd" | "pushd" | "export"))
}

/// The directory a `cd` or `pushd` command changes to, `None` for any other command. A `cd` with no argument goes home.
pub(super) fn changes_to(segment: &Segment) -> Option<String> {
    let words = &segment.words;
    matches!(shell::basename(words.first()?), "cd" | "pushd")
        .then(|| words.get(1).map_or("~", String::as_str).to_owned())
}

/// The command an argument of `concurrently` runs, `npm:dev:web` standing for `npm run dev:web`.
fn expand_shorthand(argument: &str) -> String {
    match argument.split_once(':') {
        Some((manager, script)) if guard::PACKAGE_MANAGERS.contains(&manager) => format!("{manager} run {script}"),
        _ => argument.to_owned(),
    }
}

/// The index of the first argument of `concurrently` among `words`, `None` when they are another command.
fn concurrently_arguments(words: &[String]) -> Option<usize> {
    let launched = shell::strip_launchers(words);
    let program = launched.first()?;
    matches!(shell::basename(program), "concurrently" | "conc").then(|| words.len() - launched.len() + 1)
}

/// The commands `concurrently` runs, the words of its command being `words` and its arguments starting at `first`.
/// `None` when it has options that group several letters (`-kc`), which may take a value this reading cannot place.
fn concurrent_children(words: &[String], first: usize) -> Option<Vec<Child>> {
    let mut children = Vec::new();
    let mut index = first;
    while let Some(text) = words.get(index) {
        if text == "--" {
            break;
        }
        if CONCURRENTLY_OPTIONS_WITH_VALUE.contains(&text.as_str()) {
            index += 1;
        } else if text.starts_with('-') {
            if !text.starts_with("--") && text.len() > 2 {
                return None;
            }
        } else {
            children.push(Child {
                index,
                text: text.clone(),
                recognised: starts_a_dev_server(&expand_shorthand(text)),
            });
        }
        index += 1;
    }
    Some(children)
}

/// The role of the command `segment`.
fn role(segment: &Segment) -> Role {
    let words = &segment.words;
    if sets_the_scene(segment) {
        return Role::Context;
    }
    if words.first().is_some_and(|word| shell::basename(word) == "wait") {
        return Role::Wait;
    }
    if let Some(first) = concurrently_arguments(words) {
        return concurrent_children(words, first).map_or(Role::Opaque, Role::Concurrently);
    }
    if starts_a_dev_server(&segment.text) {
        Role::DevServer
    } else {
        Role::Other
    }
}

/// How 2 commands of a list are joined.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Join {
    /// `&&`, the next command waits for the previous one to end well.
    After,
    /// `;` or a line break, the next command waits for the previous one to end.
    Then,
    /// `&`, both run side by side.
    Alongside,
}

impl Join {
    /// The join that `operator` writes, `None` for anything else than the three a list can be made of.
    fn of(operator: &str) -> Option<Self> {
        match operator {
            "&&" => Some(Self::After),
            "&" => Some(Self::Alongside),
            ";" | "" => Some(Self::Then),
            _ => None,
        }
    }

    /// The operator that writes the join.
    fn operator(self) -> &'static str {
        match self {
            Self::After => " && ",
            Self::Then => "; ",
            Self::Alongside => " & ",
        }
    }

    /// What joins 2 commands that were apart by `joins`: the loosest of them, since commands that were side by side
    /// stay so, and `&&` between 2 commands that a `&` separated would make the second wait for a server.
    fn loosest(joins: &[Self]) -> &'static str {
        if joins.contains(&Self::Alongside) {
            " & "
        } else if joins.contains(&Self::Then) {
            "; "
        } else {
            " && "
        }
    }
}

/// The commands of `script` and how each is joined to the next, `None` when the script is written with anything else
/// than `&`, `&&`, `;` and line breaks between its commands.
fn read(script: &str) -> Option<(Vec<Segment>, Vec<Join>)> {
    let (segments, joins) = split(script)?;
    let (first, last) = (segments.first()?, segments.last()?);
    let after_last = script[last.span.end..].trim_matches(|c: char| c.is_whitespace() || c == '&' || c == ';');
    (first.span.start == script.len() - script.trim_start().len() && after_last.is_empty()).then_some((segments, joins))
}

/// The commands of `script` and how each is joined to the next, a redirection staying with its command. `None` when
/// the commands are joined by anything else than `&`, `&&`, `;` and line breaks.
fn split(script: &str) -> Option<(Vec<Segment>, Vec<Join>)> {
    let mut segments: Vec<Segment> = Vec::new();
    let mut joins = Vec::new();
    for segment in shell::segments(script) {
        if let Some(previous) = segments.last_mut() {
            let operator = script[previous.span.end..segment.span.start].trim();
            // `2>&1` and `&>file` are redirections, which the splitting took for a `&`.
            if operator == "&" && (previous.text.ends_with(['>', '<']) || segment.text.starts_with(['>', '<'])) {
                previous.span.end = segment.span.end;
                script[previous.span.clone()].clone_into(&mut previous.text);
                previous.words.extend(segment.words);
                previous.word_spans.extend(segment.word_spans);
                continue;
            }
            joins.push(Join::of(operator)?);
        }
        segments.push(segment);
    }
    Some((segments, joins))
}

/// `step` as the Group runs it: the text of the command, without the arguments of `concurrently` that are left out.
fn written(script: &str, step: &Step) -> String {
    let segment = step.segment;
    let Role::Concurrently(children) = &step.role else {
        return segment.text.clone();
    };
    let words = &segment.words;
    let spans = &segment.word_spans;
    let mut edits: Vec<(Range<usize>, String)> = Vec::new();
    for child in children.iter().filter(|child| !child.recognised) {
        edits.push((spans[child.index - 1].end..spans[child.index].end, String::new()));
    }
    // Names and colors are one entry per command, so the entries of the commands left out go too.
    if !words.iter().any(|word| word.starts_with("--name-separator")) {
        let kept = |at: usize| children.get(at).is_none_or(|child| child.recognised);
        let narrow_list = |list: &str| {
            let entries: Vec<&str> = list
                .split(',')
                .enumerate()
                .filter_map(|(at, entry)| kept(at).then_some(entry))
                .collect();
            shell_word(&entries.join(","))
        };
        for (at, word) in words.iter().enumerate() {
            for (short, long) in CONCURRENTLY_LISTS {
                if word == short || word == long {
                    if let Some(list) = words.get(at + 1) {
                        edits.push((spans[at + 1].clone(), narrow_list(list)));
                    }
                } else if let Some(list) = word.strip_prefix(&format!("{long}=")) {
                    edits.push((spans[at].clone(), format!("{long}={}", narrow_list(list))));
                }
            }
        }
    }
    edits.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
    let mut text = script[segment.span.clone()].to_owned();
    for (range, replacement) in edits {
        let shifted = range.start - segment.span.start..range.end - segment.span.start;
        text.replace_range(shifted, &replacement);
    }
    text
}

/// The sentence that says a command was left out.
fn left_out(command: &str) -> String {
    format!(
        "`{command}` is left out of the Group that runs the `dev` script of the root, it is not recognised as a dev \
         server. Add it to the Group on purpose if Procly should start it."
    )
}

/// The Group the `dev` script `script` of the root calls for.
pub(super) fn narrow(script: &str) -> Narrowed {
    let Some((segments, joins)) = read(script) else {
        return Narrowed::Whole;
    };
    let steps: Vec<Step> = segments
        .iter()
        .map(|segment| Step {
            segment,
            role: role(segment),
        })
        .collect();
    if steps.len() == 1 && !matches!(steps[0].role, Role::Concurrently(_)) {
        return Narrowed::Whole;
    }
    if !steps.iter().any(Step::leaves_out) {
        return Narrowed::Whole;
    }

    // A `wait` stays as long as a command before it is run, and a command that sets the scene as long as a command after
    // it is.
    let mut runs: Vec<bool> = steps.iter().map(Step::recognised).collect();
    let mut earlier = false;
    for (at, step) in steps.iter().enumerate() {
        match step.role {
            Role::Wait => runs[at] = earlier,
            Role::Context => {}
            _ => earlier |= runs[at],
        }
    }
    let mut later = false;
    for (at, step) in steps.iter().enumerate().rev() {
        if matches!(step.role, Role::Context) {
            runs[at] = later;
        } else {
            later |= runs[at];
        }
    }

    let mut notes = Vec::new();
    for (at, step) in steps.iter().enumerate() {
        match &step.role {
            Role::Other => {
                let mut note = left_out(&step.segment.text);
                if let Some(waiting) = waiting_after(at, &steps, &joins, &runs) {
                    note = format!("{note} `{waiting}` used to run after it and now starts without waiting for it.");
                }
                notes.push(note);
            }
            Role::Concurrently(children) => {
                notes.extend(
                    children
                        .iter()
                        .filter(|child| !child.recognised)
                        .map(|child| left_out(&child.text)),
                );
            }
            Role::Context | Role::Wait | Role::DevServer | Role::Opaque => {}
        }
    }

    let Some(first) = runs.iter().position(|runs| *runs) else {
        notes.push(
            "No Group is proposed, since no command of the `dev` script of the root is recognised as a dev server. \
             The Services are started one by one."
                .to_owned(),
        );
        return Narrowed::Nothing { notes };
    };
    let mut run = written(script, &steps[first]);
    let mut previous = first;
    for at in (first + 1)..steps.len() {
        if !runs[at] {
            continue;
        }
        // A command that sets the scene is run by the shell itself, so it keeps the join it had.
        let joiner = if at == previous + 1 {
            script[steps[previous].segment.span.end..steps[at].segment.span.start].to_owned()
        } else if matches!(steps[previous].role, Role::Context) {
            joins[previous].operator().to_owned()
        } else {
            Join::loosest(&joins[previous..at]).to_owned()
        };
        run.push_str(&joiner);
        run.push_str(&written(script, &steps[at]));
        previous = at;
    }
    Narrowed::Kept {
        run,
        commands: commands(&steps, &runs),
        notes,
    }
}

/// The commands the Group runs that start a dev server, each with the directories the script changed to before it.
fn commands(steps: &[Step], runs: &[bool]) -> Vec<Command> {
    let mut directories = Vec::new();
    let mut commands = Vec::new();
    for (step, runs) in steps.iter().zip(runs) {
        if !runs {
            continue;
        }
        let mut command = |text: String| {
            commands.push(Command {
                text,
                directories: directories.clone(),
            });
        };
        match &step.role {
            Role::Context => directories.extend(changes_to(step.segment)),
            Role::DevServer | Role::Opaque => command(step.segment.text.clone()),
            Role::Concurrently(children) => {
                for child in children.iter().filter(|child| child.recognised) {
                    command(expand_shorthand(&child.text));
                }
            }
            Role::Wait | Role::Other => {}
        }
    }
    commands
}

/// The command that waited for the one at `at` to end, and still runs.
fn waiting_after(at: usize, steps: &[Step], joins: &[Join], runs: &[bool]) -> Option<String> {
    let mut next = at;
    while joins.get(next).is_some_and(|join| *join != Join::Alongside) {
        next += 1;
        if runs[next] && !matches!(steps[next].role, Role::Context | Role::Wait) {
            return Some(steps[next].segment.text.clone());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kept(script: &str) -> String {
        match narrow(script) {
            Narrowed::Kept { run, .. } => run,
            other => panic!("{script} gave {other:?}"),
        }
    }

    /// What the script gives when `left` is its only command that starts something and the Guard knows no dev server.
    fn nothing_recognised(left: &str) -> Narrowed {
        Narrowed::Nothing {
            notes: vec![
                left_out(left),
                "No Group is proposed, since no command of the `dev` script of the root is recognised as a dev \
                 server. The Services are started one by one."
                    .to_owned(),
            ],
        }
    }

    #[test]
    fn a_script_that_is_one_command_is_never_judged() {
        for script in [
            "turbo dev",
            "pnpm -r --parallel dev",
            "node worker.js",
            "npm-run-all --parallel dev:*",
        ] {
            assert_eq!(narrow(script), Narrowed::Whole, "{script}");
        }
    }

    #[test]
    fn a_list_of_dev_servers_only_is_kept_as_it_is() {
        assert_eq!(narrow("next dev & vite"), Narrowed::Whole);
        assert_eq!(narrow("cd apps/web && next dev"), Narrowed::Whole);
    }

    #[test]
    fn the_commands_joined_by_and_are_read_one_by_one() {
        assert_eq!(kept("turbo dev & stripe listen & node worker.js"), "turbo dev");
        assert_eq!(kept("stripe listen; next dev && node worker.js"), "next dev");
        assert_eq!(kept("next dev; stripe listen; vite"), "next dev; vite");
        assert_eq!(kept("next dev &\n  stripe listen\n  vite"), "next dev & vite");
    }

    #[test]
    fn what_was_side_by_side_stays_so() {
        assert_eq!(kept("next dev && stripe listen & vite"), "next dev & vite");
        assert_eq!(kept("next dev & stripe listen && vite"), "next dev & vite");
        assert_eq!(kept("next dev && stripe listen && vite"), "next dev && vite");
    }

    #[test]
    fn a_command_that_sets_the_scene_stays_for_the_commands_after_it() {
        assert_eq!(
            kept("cd apps/web && next dev & node worker.js & wait"),
            "cd apps/web && next dev & wait"
        );
        assert_eq!(
            narrow("cd apps/web && node worker.js"),
            nothing_recognised("node worker.js")
        );
    }

    #[test]
    fn a_redirection_is_part_of_its_command() {
        assert_eq!(narrow("node worker.js 2>&1"), Narrowed::Whole);
        assert_eq!(
            kept("next dev > dev.log 2>&1 & node worker.js"),
            "next dev > dev.log 2>&1"
        );
        assert_eq!(kept("next dev &> dev.log & node worker.js"), "next dev &> dev.log");
    }

    #[test]
    fn a_trailing_operator_is_dropped_with_what_it_ended() {
        assert_eq!(kept("next dev & node worker.js &"), "next dev");
        assert_eq!(kept("node worker.js; next dev;"), "next dev");
    }

    #[test]
    fn a_wait_stays_as_long_as_a_command_before_it_does() {
        assert_eq!(
            kept("next dev & vite & node worker.js & wait"),
            "next dev & vite & wait"
        );
        assert_eq!(narrow("node worker.js & wait"), nothing_recognised("node worker.js"));
    }

    #[test]
    fn a_command_that_sets_the_scene_is_never_sent_to_the_background() {
        assert_eq!(
            kept("export NODE_ENV=development; stripe listen & next dev"),
            "export NODE_ENV=development; next dev"
        );
        assert_eq!(kept("cd apps/web; stripe listen & next dev"), "cd apps/web; next dev");
    }

    #[test]
    fn an_argument_of_concurrently_that_starts_something_else_too_is_left_out() {
        assert_eq!(
            kept(r#"concurrently "stripe listen & next dev" vite"#),
            "concurrently vite"
        );
        assert_eq!(
            kept(r#"concurrently "cd apps/web && next dev" "node worker.js""#),
            r#"concurrently "cd apps/web && next dev""#
        );
    }

    #[test]
    fn options_of_concurrently_that_cannot_be_placed_leave_the_script_as_it_is() {
        assert_eq!(
            narrow(r#"concurrently -kc blue,red "next dev" "node worker.js""#),
            Narrowed::Whole
        );
        assert_eq!(
            kept(r#"concurrently "next dev" "node worker.js" -- --port 3000"#),
            r#"concurrently "next dev" -- --port 3000"#
        );
    }

    #[test]
    fn anything_else_than_a_plain_list_is_left_alone() {
        for script in [
            "next dev || node worker.js",
            "(next dev) & node worker.js",
            "next dev | cat & node a.js",
        ] {
            assert_eq!(narrow(script), Narrowed::Whole, "{script}");
        }
    }

    #[test]
    fn the_arguments_of_concurrently_are_commands_of_their_own() {
        assert_eq!(
            kept(r#"concurrently "pnpm --filter web dev" "stripe listen" 'node worker.js'"#),
            r#"concurrently "pnpm --filter web dev""#
        );
        assert_eq!(
            kept(r#"npx concurrently -k --raw "next dev" "stripe listen" "vite""#),
            r#"npx concurrently -k --raw "next dev" "vite""#
        );
        assert_eq!(kept("concurrently npm:dev:web npm:worker"), "concurrently npm:dev:web");
    }

    #[test]
    fn the_names_and_colors_of_concurrently_follow_the_commands_that_stay() {
        assert_eq!(
            kept(r#"concurrently -n web,stripe,api -c blue,red,green "next dev" "stripe listen" "node ace serve""#),
            r#"concurrently -n web,api -c blue,green "next dev" "node ace serve""#
        );
        assert_eq!(
            kept(r#"concurrently --names=web,stripe "next dev" "stripe listen""#),
            r#"concurrently --names=web "next dev""#
        );
    }

    #[test]
    fn a_left_out_command_that_a_kept_one_waited_for_says_so() {
        let Narrowed::Kept { notes, .. } = narrow("pnpm build && next dev & node worker.js") else {
            panic!("the script keeps next dev");
        };
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert!(
            notes[0].contains("`pnpm build`") && notes[0].contains("`next dev` used to run after it"),
            "{notes:?}"
        );
        assert!(!notes[1].contains("used to run after it"), "{notes:?}");
    }

    #[test]
    fn the_wait_is_noted_for_every_left_out_command_before_the_kept_one_and_for_a_semicolon_too() {
        let Narrowed::Kept { notes, .. } = narrow("pnpm build && node worker.js; next dev") else {
            panic!("the script keeps next dev");
        };
        assert!(
            notes
                .iter()
                .all(|note| note.contains("`next dev` used to run after it")),
            "{notes:?}"
        );
    }
}
