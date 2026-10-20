//! The command a Group runs once the commands of the root `dev` script that init did not recognise are left out.
//!
//! A root `dev` script that is one command, such as `turbo dev` or `pnpm -r --parallel dev`, is never judged: the
//! Group runs it through the package manager as it always did. A script that lists commands, with `&`, `&&`, `;` or
//! as the arguments of `concurrently`, is read command by command. A command is recognised when the Guard knows it as
//! a dev server, and the Group runs the recognised ones, written as the script wrote them.

use std::ops::Range;

use super::scripts::shell_word;
use crate::guard::{self, Kind, Segment};

/// What the root `dev` script becomes as the command of a Group.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Narrowed {
    /// The script is one command, or every command of it is recognised: the Group runs the script as it is.
    Whole,
    /// The Group runs `run`, and `notes` name the commands it left out.
    Kept { run: String, notes: Vec<String> },
    /// No command is recognised, so there is no Group to propose.
    Nothing { notes: Vec<String> },
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

/// The package managers whose `<manager>:<script>` shorthand `concurrently` expands.
const SHORTHAND_MANAGERS: [&str; 4] = ["npm", "pnpm", "yarn", "bun"];

/// One command `concurrently` runs, an argument of it.
#[derive(Debug)]
struct Child {
    /// The index of the argument among the words of its step.
    word: usize,
    /// The command as the argument holds it.
    text: String,
    recognised: bool,
}

#[derive(Debug)]
enum Role {
    /// A command that only sets the scene for the next ones: `cd apps/web`, `export NODE_ENV=development`, `wait`.
    Context,
    /// A command that starts a dev server.
    DevServer,
    /// A command that starts something else.
    Other,
    /// `concurrently` and the commands it runs.
    Concurrently(Vec<Child>),
}

#[derive(Debug)]
struct Step<'a> {
    segment: &'a Segment,
    role: Role,
}

impl Step<'_> {
    /// Whether the Group runs the step, its context aside.
    fn recognised(&self) -> bool {
        match &self.role {
            Role::DevServer => true,
            Role::Context | Role::Other => false,
            Role::Concurrently(children) => children.iter().any(|child| child.recognised),
        }
    }

    /// Whether the step leaves out something the script starts.
    fn leaves_out(&self) -> bool {
        match &self.role {
            Role::Other => true,
            Role::Concurrently(children) => children.iter().any(|child| !child.recognised),
            Role::Context | Role::DevServer => false,
        }
    }
}

/// Whether the Guard knows `command` as a dev server.
fn starts_a_dev_server(command: &str) -> bool {
    guard::recognize(command).is_some_and(|recognized| recognized.kind == Kind::DevServer)
}

/// The command an argument of `concurrently` runs, `npm:dev:web` standing for `npm run dev:web`.
fn expand_shorthand(argument: &str) -> String {
    match argument.split_once(':') {
        Some((manager, script)) if SHORTHAND_MANAGERS.contains(&manager) => format!("{manager} run {script}"),
        _ => argument.to_owned(),
    }
}

/// The commands `concurrently` runs when `words` are its command, `None` for any other command.
fn concurrent_children(words: &[String]) -> Option<Vec<Child>> {
    let launched = guard::strip_launchers(words);
    let program = launched.first()?;
    if !matches!(guard::basename(program), "concurrently" | "conc") {
        return None;
    }
    let mut children = Vec::new();
    let mut word = words.len() - launched.len() + 1;
    while let Some(text) = words.get(word) {
        if CONCURRENTLY_OPTIONS_WITH_VALUE.contains(&text.as_str()) {
            word += 1;
        } else if !text.starts_with('-') {
            children.push(Child {
                word,
                text: text.clone(),
                recognised: starts_a_dev_server(&expand_shorthand(text)),
            });
        }
        word += 1;
    }
    Some(children)
}

fn role(segment: &Segment) -> Role {
    let words = &segment.words;
    let context = words.iter().all(|word| guard::is_assignment(word))
        || words
            .first()
            .is_some_and(|word| matches!(guard::basename(word), "cd" | "pushd" | "export" | "wait"));
    if context {
        return Role::Context;
    }
    match concurrent_children(words) {
        Some(children) => Role::Concurrently(children),
        None if starts_a_dev_server(&segment.text) => Role::DevServer,
        None => Role::Other,
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

/// The commands of `script` and how they are joined, `None` when the script is one command, or is written with
/// anything else than `&`, `&&`, `;` and line breaks between its commands.
fn read(script: &str) -> Option<(Vec<Segment>, Vec<Join>)> {
    let segments = guard::segments(script);
    let (first, last) = (segments.first()?, segments.last()?);
    if first.span.start != script.len() - script.trim_start().len() || last.span.end != script.trim_end().len() {
        return None;
    }
    let mut joins = Vec::new();
    for pair in segments.windows(2) {
        // `2>&1` is a redirection, which the splitting took for a `&`.
        if pair[0].text.ends_with(['>', '<']) || pair[1].text.starts_with(['>', '<']) {
            return None;
        }
        joins.push(Join::of(script[pair[0].span.end..pair[1].span.start].trim())?);
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
        edits.push((spans[child.word - 1].end..spans[child.word].end, String::new()));
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

    // A command that sets the scene stays for as long as a command after it is run.
    let mut runs: Vec<bool> = steps.iter().map(Step::recognised).collect();
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
            Role::Context | Role::DevServer => {}
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
        let joiner = if at == previous + 1 {
            script[steps[previous].segment.span.end..steps[at].segment.span.start].to_owned()
        } else {
            Join::loosest(&joins[previous..at]).to_owned()
        };
        run.push_str(&joiner);
        run.push_str(&written(script, &steps[at]));
        previous = at;
    }
    Narrowed::Kept { run, notes }
}

/// The command that waited for the one at `at` to end well, and still runs.
fn waiting_after(at: usize, steps: &[Step], joins: &[Join], runs: &[bool]) -> Option<String> {
    let mut next = at;
    while joins.get(next) == Some(&Join::After) {
        next += 1;
        match steps[next].role {
            Role::Context => {}
            Role::DevServer | Role::Concurrently(_) if runs[next] => return Some(steps[next].segment.text.clone()),
            _ => return None,
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
            "cd apps/web && next dev"
        );
        assert_eq!(
            narrow("cd apps/web && node worker.js"),
            Narrowed::Nothing {
                notes: vec![
                    left_out("node worker.js"),
                    "No Group is proposed, since no command of the `dev` script of the root is recognised as a dev \
                     server. The Services are started one by one."
                        .to_owned()
                ]
            }
        );
    }

    #[test]
    fn a_redirection_is_not_a_list() {
        assert_eq!(narrow("node worker.js 2>&1"), Narrowed::Whole);
        assert_eq!(narrow("next dev 2>&1 & node worker.js"), Narrowed::Whole);
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
}
