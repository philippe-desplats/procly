//! The Services a narrowed Group covers: the ones the commands it keeps start.
//!
//! A Group must cover exactly the Services its command starts. Once init has left commands out of the root `dev`
//! script, the Group no longer starts every package, so each command it keeps is tied to the packages it starts. A
//! command that names one, with a filter, a workspace option, a directory or a `cd`, is tied to it. A command that runs
//! the `dev` script of the whole Worktree is tied to all of them. Any other command is not tied, and init says so
//! rather than guess.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::group::{self, Command};
use crate::guard;
use crate::shell::{self, Resolved};

/// A workspace package of the Worktree that init proposes a Service for.
#[derive(Debug)]
pub(super) struct Member {
    /// The name of the Service.
    pub(super) service: String,
    /// Where the package sits, relative to the root of the Worktree.
    pub(super) dir: PathBuf,
    /// The `name` of its `package.json`, which a filter selects it by.
    pub(super) package: String,
}

/// The names of the Services the `commands` of a Group start, in name order. When a command cannot be tied to a
/// Service, the notes that say so and that no Group is proposed.
pub(super) fn covered(commands: &[Command], members: &[Member]) -> Result<Vec<String>, Vec<String>> {
    let mut services = BTreeSet::new();
    let mut notes = Vec::new();
    for command in commands {
        match tie(command, members) {
            Ok(tied) => services.extend(tied.into_iter().map(|at| members[at].service.clone())),
            Err(reason) => notes.push(format!("`{}` cannot be tied to one Service: {reason}.", command.text)),
        }
    }
    if notes.is_empty() && !services.is_empty() {
        return Ok(services.into_iter().collect());
    }
    notes.push(
        "No Group is proposed, since a Group covers exactly the Services its command starts, and what the `dev` script \
         of the root starts cannot be told. The Services are started one by one."
            .to_owned(),
    );
    Err(notes)
}

/// The members `command` starts, as indexes, or why it cannot be told.
fn tie(command: &Command, members: &[Member]) -> Result<Vec<usize>, String> {
    let mut place = Some(PathBuf::new());
    for directory in &command.directories {
        place = place.and_then(|place| resolve(&place, directory));
    }
    let mut tied = Vec::new();
    for segment in shell::segments(&command.text) {
        if let Some(directory) = group::changes_to(&segment) {
            place = place.and_then(|place| resolve(&place, &directory));
        } else if !group::sets_the_scene(&segment) {
            let place = place
                .as_deref()
                .ok_or("it runs from a directory outside the Worktree")?;
            tied.extend(tie_segment(shell::strip_launchers(&segment.words), place, members)?);
        }
    }
    Ok(tied)
}

/// The members the command `words` starts when it runs from `place`.
fn tie_segment(words: &[String], place: &Path, members: &[Member]) -> Result<Vec<usize>, String> {
    let Some((program, args)) = words.split_first() else {
        return Err("it is empty".to_owned());
    };
    let program = shell::basename(program);
    let mut filters = Vec::new();
    let mut directory = None;
    if guard::RUNNERS.contains(&program) {
        let mut options = args.iter().map(String::as_str);
        while let Some(word) = options.next() {
            match word {
                "--filter" | "-F" | "--workspace" => filters.extend(options.next()),
                "-w" if program == "npm" => filters.extend(options.next()),
                "--dir" | "-C" | "--prefix" | "--cwd" => directory = options.next(),
                _ => {
                    if let Some(value) = word
                        .strip_prefix("--filter=")
                        .or_else(|| word.strip_prefix("--workspace="))
                    {
                        filters.push(value);
                    } else if let Some(value) = word.strip_prefix("--dir=").or_else(|| word.strip_prefix("--prefix=")) {
                        directory = Some(value);
                    }
                }
            }
        }
        if let ("yarn", ["workspace", package, ..]) =
            (program, shell::positional(args, &guard::PACKAGE_OPTIONS).as_slice())
        {
            filters.push(package);
        }
    }
    if !filters.is_empty() {
        return filters
            .into_iter()
            .map(|filter| filtered(filter, place, members))
            .collect();
    }
    let place = match directory {
        Some(directory) => resolve(place, directory).ok_or("it runs from a directory outside the Worktree")?,
        None => place.to_path_buf(),
    };
    if let Some(at) = members.iter().position(|member| member.dir == place) {
        return Ok(vec![at]);
    }
    if directory.is_none() && place.as_os_str().is_empty() && guard::runs_the_whole_repository(words) {
        return Ok((0..members.len()).collect());
    }
    if place.as_os_str().is_empty() {
        return Err("it names no package and runs from the root".to_owned());
    }
    Err(format!("it runs from `{}`, which holds no Service", place.display()))
}

/// The member that the filter `value` selects.
fn filtered(value: &str, place: &Path, members: &[Member]) -> Result<usize, String> {
    if value.contains(['*', '?', '[', ']', '{', '}', '!']) || value.contains("...") {
        return Err(format!("its filter `{value}` selects by pattern"));
    }
    if value == "." || value.starts_with("./") || value.starts_with("../") {
        let dir = resolve(place, value).ok_or("its filter leaves the Worktree")?;
        return members
            .iter()
            .position(|member| member.dir == dir)
            .ok_or_else(|| format!("`{value}` is not the directory of a package with a `dev` script"));
    }
    members
        .iter()
        .position(|member| member.package == value)
        .ok_or_else(|| format!("`{value}` is not a package with a `dev` script"))
}

/// `path` as a directory of the Worktree, `base` being where it is read from. `None` outside the Worktree.
fn resolve(base: &Path, path: &str) -> Option<PathBuf> {
    match shell::resolve(base, path) {
        Resolved::Inside(resolved) => Some(resolved),
        Resolved::Above(_) | Resolved::Absolute(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn members() -> Vec<Member> {
        [("web", "apps/web", "@acme/web"), ("api", "apps/api", "api")]
            .into_iter()
            .map(|(service, dir, package)| Member {
                service: service.to_owned(),
                dir: PathBuf::from(dir),
                package: package.to_owned(),
            })
            .collect()
    }

    fn command(text: &str, directories: &[&str]) -> Command {
        Command {
            text: text.to_owned(),
            directories: directories.iter().map(|directory| (*directory).to_owned()).collect(),
        }
    }

    fn services(text: &str, directories: &[&str]) -> Result<Vec<String>, Vec<String>> {
        covered(&[command(text, directories)], &members())
    }

    #[test]
    fn a_command_that_names_a_package_covers_its_service() {
        for text in [
            "pnpm --filter @acme/web dev",
            "pnpm -F @acme/web dev",
            "pnpm --filter=@acme/web dev",
            "pnpm --filter ./apps/web dev",
            "pnpm --dir apps/web dev",
            "turbo run dev --filter=@acme/web",
            "bun run --filter @acme/web dev",
            "yarn workspace @acme/web dev",
            "npm run dev --workspace=@acme/web",
            "npm run dev -w @acme/web",
        ] {
            assert_eq!(services(text, &[]), Ok(vec!["web".to_owned()]), "{text}");
        }
    }

    #[test]
    fn the_options_that_narrow_a_command_for_the_guard_do_for_yarn_workspace_too() {
        for text in [
            "yarn --cwd apps/api workspace @acme/web dev",
            "yarn -w apps/api workspace @acme/web dev",
        ] {
            assert_eq!(services(text, &[]), Ok(vec!["web".to_owned()]), "{text}");
        }
    }

    #[test]
    fn a_command_run_from_the_directory_of_a_package_covers_its_service() {
        assert_eq!(services("next dev", &["apps/web"]), Ok(vec!["web".to_owned()]));
        assert_eq!(services("next dev", &["apps", "./api"]), Ok(vec!["api".to_owned()]));
        assert_eq!(services("cd apps/api && next dev", &[]), Ok(vec!["api".to_owned()]));
        assert_eq!(services("pnpm dev", &["apps/web"]), Ok(vec!["web".to_owned()]));
    }

    #[test]
    fn a_command_that_runs_the_dev_script_of_the_whole_worktree_covers_every_service() {
        let both = Ok(vec!["api".to_owned(), "web".to_owned()]);
        for text in ["turbo dev", "turbo run dev", "pnpm -r --parallel dev", "npm run dev"] {
            assert_eq!(services(text, &[]), both, "{text}");
        }
    }

    #[test]
    fn the_services_of_all_the_commands_are_covered_once() {
        let commands = [
            command("pnpm --filter api dev", &[]),
            command("pnpm --filter @acme/web dev", &[]),
            command("pnpm --filter api dev", &[]),
        ];
        assert_eq!(
            covered(&commands, &members()),
            Ok(vec!["api".to_owned(), "web".to_owned()])
        );
    }

    #[test]
    fn a_command_that_names_no_package_is_not_tied_and_no_group_is_proposed() {
        for (text, reason) in [
            ("next dev", "it names no package and runs from the root"),
            (
                "pnpm --filter \"@acme/*\" dev",
                "its filter `@acme/*` selects by pattern",
            ),
            ("pnpm --filter ...api dev", "its filter `...api` selects by pattern"),
            ("pnpm --filter web dev", "`web` is not a package with a `dev` script"),
            (
                "pnpm --filter ./apps/docs dev",
                "`./apps/docs` is not the directory of a package with a `dev` script",
            ),
            ("npm run dev:web", "it names no package and runs from the root"),
            ("cd tools && next dev", "it runs from `tools`, which holds no Service"),
            ("cd ../.. && next dev", "it runs from a directory outside the Worktree"),
        ] {
            let Err(notes) = services(text, &[]) else {
                panic!("{text} was tied");
            };
            assert_eq!(
                notes[0],
                format!("`{text}` cannot be tied to one Service: {reason}."),
                "{text}"
            );
            assert!(notes.last().unwrap().starts_with("No Group is proposed"), "{text}");
        }
    }

    #[test]
    fn a_path_above_the_root_of_the_worktree_is_no_directory_of_it() {
        assert_eq!(
            resolve(Path::new("apps/web"), "../api"),
            Some(PathBuf::from("apps/api"))
        );
        assert_eq!(resolve(Path::new(""), ".."), None);
        assert_eq!(resolve(Path::new("apps/web"), "../../.."), None);
        assert_eq!(resolve(Path::new("apps"), "/srv/app"), None);
        assert_eq!(resolve(Path::new("apps"), "/"), None);
    }

    #[test]
    fn one_command_that_is_not_tied_leaves_no_group_even_when_another_is() {
        let commands = [command("pnpm --filter api dev", &[]), command("next dev", &[])];
        let Err(notes) = covered(&commands, &members()) else {
            panic!("the Group was proposed");
        };
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert!(notes[0].starts_with("`next dev` cannot be tied"), "{notes:?}");
    }
}
