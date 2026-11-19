//! `procly doctor`: checks that Procly can work here and tells, for each problem, the command that fixes it.
//!
//! Doctor changes nothing and runs none of the commands it names. It runs the checks that need only the machine, the
//! state directory and the Worktree itself, and asks the Daemon for the ones only it can answer, so that an
//! unreachable Daemon is a problem it reports and not the end of the run. It never starts a Daemon.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use procly_client::ClientError;

use crate::client;
use crate::config;
use crate::daemon;
use crate::driver::{self, Driver};
use crate::guard;
use crate::protocol::{Check, CheckStatus, DaemonChecks, DecodeError, PROTOCOL_VERSION, Request, Response};
use crate::state_dir::StateDir;
use crate::topology;
use crate::unit::{Unit, UnitError};

/// The check of `docker compose`.
const DOCKER_COMPOSE: &str = "docker_compose";
/// The check of the Daemon, and of its version.
const DAEMON: &str = "daemon";
/// The check of the Session unit.
const SESSION_UNIT: &str = "session_unit";
/// The check of the `procly.toml` of the Worktree and of the Drivers it names.
const CONFIGURATION: &str = "configuration";
/// The check of the compose file the Compose services of the Worktree run from.
const COMPOSE_FILE: &str = "compose_file";
/// The directories the look for a compose file outside the root never enters.
const SKIPPED_DIRECTORIES: [&str; 3] = ["node_modules", "target", "vendor"];

/// Every check, in the order they are listed.
#[derive(Debug)]
pub(crate) struct Report {
    pub(crate) checks: Vec<Check>,
    /// The version of procly the running Daemon runs, `None` when no Daemon answered the checks.
    pub(crate) daemon_version: Option<String>,
}

impl Report {
    /// Whether some check found a problem.
    pub(crate) fn failed(&self) -> bool {
        self.checks.iter().any(|check| check.status == CheckStatus::Failed)
    }
}

/// The Worktree holding the directory `doctor` runs in.
struct Placement {
    /// `None` when the directory is in no git Repository, or git cannot tell.
    worktree: Option<PathBuf>,
    /// Why there is no Worktree.
    unplaced: String,
}

/// Runs every check from the directory `cwd`. A later check is appended to the list in [`run`].
pub(crate) fn run(state: &StateDir, cwd: &Path) -> Report {
    let placement = place(cwd);
    let worktree = placement.worktree.as_deref();
    let answer = client::send_if_running(state, &Request::Doctor);
    let daemon_version = match &answer {
        Ok(Response::Doctor(DaemonChecks { procly_version, .. })) => Some(procly_version.clone()),
        _ => None,
    };
    let (daemon_check, daemon_checks) = daemon_checks(state, answer);
    let mut checks = vec![
        docker_compose(),
        daemon_check,
        session_unit(state),
        daemon::doctor::state_documents(state),
        configuration(worktree, &placement.unplaced),
        compose_file(worktree, &placement.unplaced),
        daemon::doctor::guard(state, worktree, &placement.unplaced),
        portless(worktree, &placement.unplaced),
        daemon::doctor::worktrunk_hooks(state, worktree, &placement.unplaced),
    ];
    checks.extend(daemon_checks);
    Report { checks, daemon_version }
}

fn place(cwd: &Path) -> Placement {
    let resolved = topology::canonicalize(cwd)
        .and_then(|cwd| topology::resolve(&cwd))
        .map_err(|error| format!("{:#}", anyhow::Error::from(error)));
    match resolved {
        Ok(Some(topology)) => Placement {
            worktree: Some(topology.worktree.path),
            unplaced: String::new(),
        },
        Ok(None) => Placement {
            worktree: None,
            unplaced: "this directory is in no git Repository".to_owned(),
        },
        Err(error) => Placement {
            worktree: None,
            unplaced: format!("cannot place this directory in a Worktree: {error}"),
        },
    }
}

/// The check of the Daemon, then the checks it answered, or the same ones skipped when it did not.
fn daemon_checks(state: &StateDir, answer: Result<Response, ClientError>) -> (Check, Vec<Check>) {
    let skipped = |why: &str| {
        daemon::doctor::DAEMON_CHECKS
            .iter()
            .map(|name| Check::skipped(name, why))
            .collect()
    };
    let restart = Some("procly daemon stop".to_owned());
    match answer {
        Ok(Response::Doctor(DaemonChecks {
            procly_version: version,
            checks,
        })) => {
            let own = env!("CARGO_PKG_VERSION");
            if version == own {
                (
                    Check::passed(DAEMON, format!("the Daemon answers and runs procly {version}")),
                    checks,
                )
            } else {
                (
                    Check::failed(
                        DAEMON,
                        format!("the Daemon runs procly {version}, this is procly {own}, restart it to run this one"),
                        restart,
                    ),
                    checks,
                )
            }
        }
        // A Daemon of another protocol version answers with the version it speaks, whatever it was asked.
        Err(ClientError::Decode(DecodeError::Version(version))) => (
            Check::failed(
                DAEMON,
                format!(
                    "the Daemon speaks protocol version {version}, this procly speaks version {PROTOCOL_VERSION}, \
                     restart it to run this one"
                ),
                restart,
            ),
            skipped("the Daemon speaks another protocol version"),
        ),
        Ok(Response::Error { message }) => (
            Check::failed(DAEMON, format!("the Daemon failed: {message}"), None),
            skipped("the Daemon failed"),
        ),
        Ok(other) => (
            Check::failed(DAEMON, format!("unexpected answer from the Daemon: {other:?}"), None),
            skipped("the Daemon gave an unexpected answer"),
        ),
        // Doctor never starts a Daemon, which would reconcile what the last one left. Any other command does.
        Err(ClientError::Unreachable { path, .. }) => (
            Check::failed(
                DAEMON,
                format!(
                    "no Daemon runs, nothing answers on {}, any procly command starts one",
                    path.display()
                ),
                Some("procly status".to_owned()),
            ),
            skipped("no Daemon runs"),
        ),
        Err(error) => {
            let fix = matches!(error, ClientError::Connection { .. } | ClientError::NoResponse)
                .then(|| format!("tail -n 20 {}", state.log().display()));
            (
                Check::failed(DAEMON, format!("{:#}", anyhow::Error::from(error)), fix),
                skipped("the Daemon did not answer"),
            )
        }
    }
}

/// Whether `docker compose` v2 runs, which the Compose services need.
fn docker_compose() -> Check {
    // Docker Desktop is the usual way to get it on a Mac, elsewhere it depends on the distribution.
    let install = || cfg!(target_os = "macos").then(|| "brew install --cask docker".to_owned());
    let output = Command::new("docker")
        .args(["compose", "version", "--short"])
        .stdin(Stdio::null())
        .output();
    let output = match output {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Check::failed(
                DOCKER_COMPOSE,
                "docker is not on the PATH, install Docker with the Compose v2 plugin",
                install(),
            );
        }
        Err(error) => return Check::failed(DOCKER_COMPOSE, format!("cannot run docker: {error}"), install()),
    };
    if !output.status.success() {
        return Check::failed(
            DOCKER_COMPOSE,
            format!("docker compose v2 is unavailable: {}", first_line(&output.stderr)),
            install(),
        );
    }
    let version = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    match daemon::compose_major(&version) {
        Some(major) if major < 2 => Check::failed(
            DOCKER_COMPOSE,
            format!("found docker compose {version}, Procly needs v2"),
            install(),
        ),
        _ => Check::passed(DOCKER_COMPOSE, format!("docker compose {version}")),
    }
}

/// The first line of `stderr` that says something, which is where a command tells why it failed.
fn first_line(stderr: &[u8]) -> String {
    String::from_utf8_lossy(stderr)
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("it printed nothing")
        .to_owned()
}

/// Whether the Session unit is installed and loaded, so that the Daemon comes back with the session.
fn session_unit(state: &StateDir) -> Check {
    let install = Some("procly unit install".to_owned());
    let unit = match Unit::resolve(state) {
        Ok(unit) => unit,
        Err(UnitError::Unsupported) => return Check::skipped(SESSION_UNIT, UnitError::Unsupported.to_string()),
        Err(error) => return Check::failed(SESSION_UNIT, format!("{:#}", anyhow::Error::from(error)), None),
    };
    // The session manager is asked only about a unit this command resolved and found, never about one of another home.
    if !unit.path.exists() {
        return Check::warning(
            SESSION_UNIT,
            format!(
                "the Session unit {} is not installed, the Daemon does not start with your session",
                unit.path.display()
            ),
            install,
        );
    }
    match unit.inspect() {
        Ok(inspection) if inspection.enabled => Check::passed(
            SESSION_UNIT,
            format!("the Session unit {} is installed and loaded", unit.path.display()),
        ),
        Ok(_) => Check::failed(
            SESSION_UNIT,
            format!(
                "the Session unit {} is installed but not loaded, the Daemon does not start with your session",
                unit.path.display()
            ),
            install,
        ),
        Err(error) => Check::failed(SESSION_UNIT, format!("{:#}", anyhow::Error::from(error)), None),
    }
}

/// Whether the `procly.toml` of the Worktree is valid and every Driver it names loads.
fn configuration(worktree: Option<&Path>, unplaced: &str) -> Check {
    let Some(worktree) = worktree else {
        return Check::skipped(CONFIGURATION, unplaced);
    };
    let file = worktree.join(config::VERSIONED_FILE);
    if !file.exists() {
        return Check::skipped(
            CONFIGURATION,
            format!(
                "{} has no {}, `procly init` proposes one",
                worktree.display(),
                config::VERSIONED_FILE
            ),
        );
    }
    let edit = |path: &Path| format!("${{EDITOR:-vi}} {}", path.display());
    let config = match config::load(config::user_settings().as_deref(), worktree) {
        Ok(config) => config,
        Err(error) => {
            return Check::failed(
                CONFIGURATION,
                format!("{:#}", anyhow::Error::from(error)),
                Some(edit(&file)),
            );
        }
    };
    let user_dir = driver::user_dir();
    let mut problems = Vec::new();
    let mut fixes = Vec::new();
    for (service, declared) in &config.services {
        let Some(name) = &declared.driver else { continue };
        if let Err(error) = Driver::load(name, worktree, user_dir.as_deref()) {
            problems.push(format!("service {service}: {:#}", anyhow::Error::from(error)));
            fixes.push(edit(
                &worktree.join(driver::REPOSITORY_DIR).join(format!("{name}.toml")),
            ));
        }
    }
    if problems.is_empty() {
        return Check::passed(
            CONFIGURATION,
            format!("{} is valid and its Drivers load", file.display()),
        );
    }
    fixes.sort();
    fixes.dedup();
    Check::failed(CONFIGURATION, problems.join("\n"), Some(fixes.join("\n")))
}

/// Whether the compose file the Compose services of the Worktree run from can be found.
fn compose_file(worktree: Option<&Path>, unplaced: &str) -> Check {
    let Some(worktree) = worktree else {
        return Check::skipped(COMPOSE_FILE, unplaced);
    };
    if !worktree.join(config::VERSIONED_FILE).exists() {
        return Check::skipped(
            COMPOSE_FILE,
            format!("{} has no {}", worktree.display(), config::VERSIONED_FILE),
        );
    }
    match config::load(config::user_settings().as_deref(), worktree) {
        Ok(config) => compose_file_check(worktree, &config),
        Err(_) => Check::skipped(
            COMPOSE_FILE,
            "the configuration cannot be read, see the configuration check",
        ),
    }
}

/// Whether the Compose services `config` declares in `worktree` find the compose file `settings.compose_file` declares.
/// Without the setting, `docker compose` finds its compose file at the root or through a `COMPOSE_FILE` doctor does
/// not read, so the check never fails: it is skipped, and names the setting that declares the compose files it finds
/// below a root that holds none.
fn compose_file_check(worktree: &Path, config: &config::Config) -> Check {
    let edit = format!("${{EDITOR:-vi}} {}", worktree.join(config::VERSIONED_FILE).display());
    let services: Vec<&str> = config
        .services
        .iter()
        .filter(|(_, declared)| declared.compose.is_some())
        .map(|(name, _)| name.as_str())
        .collect();
    let at_root = guard::COMPOSE_FILES
        .iter()
        .map(|name| worktree.join(name))
        .find(|file| file.is_file());
    // Named only when nothing the Daemon would read is there.
    let declaring = || {
        let below = compose_files_below(worktree);
        below
            .first()
            .map(|first| {
                format!(
                    ", declare {} with `compose_file = \"{}\"` in the [settings] of {}",
                    below
                        .iter()
                        .map(|file| file.display().to_string())
                        .collect::<Vec<_>>()
                        .join(" or "),
                    first.display(),
                    config::VERSIONED_FILE
                )
            })
            .unwrap_or_default()
    };
    let Some(declared) = &config.settings.compose_file else {
        let setting = if at_root.is_none() { declaring() } else { String::new() };
        let why = if services.is_empty() {
            "no Compose service is declared, none needs a compose file".to_owned()
        } else {
            "settings.compose_file is not set, the Compose services run from the compose file docker compose finds at \
             the root of the Worktree or through COMPOSE_FILE"
                .to_owned()
        };
        return Check::skipped(COMPOSE_FILE, format!("{why}{setting}"));
    };
    if services.is_empty() {
        return Check::skipped(
            COMPOSE_FILE,
            "no Compose service is declared, none needs a compose file",
        );
    }
    let services = services.join(", ");
    let file = worktree.join(declared);
    if file.is_file() {
        Check::passed(
            COMPOSE_FILE,
            format!("the Compose services {services} run from {}", file.display()),
        )
    } else {
        Check::failed(
            COMPOSE_FILE,
            format!(
                "{} does not exist, the compose file settings.compose_file declares, which the Compose services \
                 {services} run from",
                file.display()
            ),
            Some(edit),
        )
    }
}

/// The compose files below the root of `worktree`, in its directories and theirs, relative to it and sorted. Hidden
/// directories and those of dependencies and builds are not looked into.
fn compose_files_below(worktree: &Path) -> Vec<PathBuf> {
    let directories = |parent: &Path| -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(worktree.join(parent)) else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .map(|entry| entry.file_name())
            .filter(|name| {
                let name = name.to_string_lossy();
                !name.starts_with('.') && !SKIPPED_DIRECTORIES.contains(&name.as_ref())
            })
            .map(|name| parent.join(name))
            .collect()
    };
    let children = directories(Path::new(""));
    let grandchildren: Vec<PathBuf> = children.iter().flat_map(|child| directories(child)).collect();
    let mut found: Vec<PathBuf> = children
        .iter()
        .chain(&grandchildren)
        .filter_map(|directory| {
            guard::COMPOSE_FILES
                .iter()
                .map(|name| directory.join(name))
                .find(|file| worktree.join(file).is_file())
        })
        .collect();
    found.sort();
    found
}

/// Whether Portless is on for the Worktree and its `portless` executable is on the `PATH`.
fn portless(worktree: Option<&Path>, unplaced: &str) -> Check {
    let Some(worktree) = worktree else {
        return Check::skipped(daemon::doctor::PORTLESS, unplaced);
    };
    if !worktree.join(config::VERSIONED_FILE).exists() {
        return Check::skipped(
            daemon::doctor::PORTLESS,
            format!("{} has no {}", worktree.display(), config::VERSIONED_FILE),
        );
    }
    let on = config::load(config::user_settings().as_deref(), worktree)
        .ok()
        .map(|config| config.settings.portless);
    daemon::doctor::portless_binary(on)
}

/// What `doctor` prints: one line per check, its further lines indented, then the fix of a problem.
pub(crate) fn text(report: &Report) -> String {
    let mut text = String::new();
    for check in &report.checks {
        let mut lines = check.message.lines();
        let _ = writeln!(
            text,
            "{:<7} {}: {}",
            check.status.as_str(),
            check.name,
            lines.next().unwrap_or_default()
        );
        for line in lines {
            let _ = writeln!(text, "        {line}");
        }
        for fix in check.fix.iter().flat_map(|fix| fix.lines()) {
            let _ = writeln!(text, "        fix: {fix}");
        }
    }
    text
}

/// What `doctor --agent` hands to the coding agent: each failed check with the command that fixes it. `None` when no
/// check failed, since there is then nothing to fix and no agent is started.
pub(crate) fn prompt(report: &Report) -> Option<String> {
    let failed: Vec<&Check> = report
        .checks
        .iter()
        .filter(|check| check.status == CheckStatus::Failed)
        .collect();
    if failed.is_empty() {
        return None;
    }
    let mut prompt = format!(
        "`procly doctor` checked this machine and found {} {} listed below. Fix them in the order listed. For each \
         problem, run the commands under it, one after the other, then say what you did. Run no other command and \
         change nothing else. Do not start a Procly Daemon yourself. A problem with no command is one no command \
         fixes: explain what is wrong and what you would do, and change nothing.\n",
        failed.len(),
        if failed.len() == 1 { "problem" } else { "problems" }
    );
    for (index, check) in failed.iter().enumerate() {
        let mut lines = check.message.lines();
        let _ = write!(
            prompt,
            "\n{}. {}: {}\n",
            index + 1,
            check.name,
            lines.next().unwrap_or_default()
        );
        for line in lines {
            let _ = writeln!(prompt, "   {line}");
        }
        match &check.fix {
            Some(fix) => {
                let _ = writeln!(prompt, "   Commands:");
                for line in fix.lines() {
                    let _ = writeln!(prompt, "   - {line}");
                }
            }
            None => {
                let _ = writeln!(prompt, "   No command fixes it.");
            }
        }
    }
    Some(prompt)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(status: CheckStatus, message: &str, fix: Option<&str>) -> Check {
        Check {
            name: "leased_ports".to_owned(),
            status,
            message: message.to_owned(),
            fix: fix.map(str::to_owned),
        }
    }

    #[test]
    fn a_report_with_a_failed_check_has_failed_and_one_with_only_skipped_or_warning_ones_has_not() {
        let report = |statuses: &[CheckStatus]| Report {
            checks: statuses.iter().map(|&status| check(status, "m", None)).collect(),
            daemon_version: None,
        };
        assert!(!report(&[CheckStatus::Passed, CheckStatus::Skipped, CheckStatus::Warning]).failed());
        assert!(report(&[CheckStatus::Passed, CheckStatus::Failed]).failed());
    }

    #[test]
    fn the_text_gives_a_check_one_line_with_its_further_lines_and_each_fix_indented_under_it() {
        let report = Report {
            checks: vec![
                check(CheckStatus::Passed, "all good", None),
                check(CheckStatus::Failed, "one\ntwo", Some("kill 1\nkill 2")),
                check(CheckStatus::Warning, "missing", Some("install it")),
            ],
            daemon_version: None,
        };
        assert_eq!(
            text(&report),
            "passed  leased_ports: all good\n\
             failed  leased_ports: one\n        two\n        fix: kill 1\n        fix: kill 2\n\
             warning leased_ports: missing\n        fix: install it\n"
        );
    }

    #[test]
    fn the_prompt_lists_each_failed_check_with_its_commands_and_leaves_out_the_others() {
        let report = Report {
            checks: vec![
                check(CheckStatus::Passed, "all good", None),
                check(CheckStatus::Warning, "missing", Some("install it")),
                check(CheckStatus::Failed, "one\ntwo", Some("kill 1\nkill 2")),
                check(CheckStatus::Failed, "no way out", None),
            ],
            daemon_version: None,
        };
        let prompt = prompt(&report).expect("two checks failed");
        assert!(prompt.contains("found 2 problems"), "{prompt}");
        assert!(
            prompt.contains("1. leased_ports: one\n   two\n   Commands:\n   - kill 1\n   - kill 2\n"),
            "{prompt}"
        );
        assert!(
            prompt.contains("2. leased_ports: no way out\n   No command fixes it.\n"),
            "{prompt}"
        );
        assert!(!prompt.contains("install it"), "{prompt}");
    }

    #[test]
    fn there_is_no_prompt_when_no_check_failed() {
        let report = Report {
            checks: vec![
                check(CheckStatus::Passed, "all good", None),
                check(CheckStatus::Skipped, "n/a", None),
                check(CheckStatus::Warning, "missing", Some("install it")),
            ],
            daemon_version: None,
        };
        assert_eq!(prompt(&report), None);
    }

    /// The configuration `text` of a Worktree, loaded without user settings.
    fn loaded(worktree: &Path, text: &str) -> config::Config {
        std::fs::write(worktree.join(config::VERSIONED_FILE), text).unwrap();
        config::load(None, worktree).unwrap()
    }

    const COMPOSE_SERVICES: &str = "[services.db]\ncompose = 'postgres'\n[services.cache]\ncompose = 'redis'\n";

    #[test]
    fn a_compose_service_whose_compose_file_is_missing_fails_naming_the_path_looked_at() {
        let dir = tempfile::tempdir().unwrap();
        let worktree = dir.path();
        let declared = format!("[settings]\ncompose_file = 'apps/api/docker-compose.yml'\n{COMPOSE_SERVICES}");

        let missing = compose_file_check(worktree, &loaded(worktree, &declared));
        assert_eq!(missing.status, CheckStatus::Failed, "{}", missing.message);
        assert!(
            missing.message.contains(&format!(
                "{} does not exist",
                worktree.join("apps/api/docker-compose.yml").display()
            )) && missing.message.contains("cache, db"),
            "{}",
            missing.message
        );
        assert_eq!(
            missing.fix,
            Some(format!("${{EDITOR:-vi}} {}", worktree.join("procly.toml").display()))
        );

        std::fs::create_dir_all(worktree.join("apps/api")).unwrap();
        std::fs::write(worktree.join("apps/api/docker-compose.yml"), "services: {}\n").unwrap();
        let found = compose_file_check(worktree, &loaded(worktree, &declared));
        assert_eq!(found.status, CheckStatus::Passed, "{}", found.message);
        assert!(
            found
                .message
                .contains(&worktree.join("apps/api/docker-compose.yml").display().to_string()),
            "{}",
            found.message
        );
    }

    #[test]
    fn without_the_setting_the_check_never_fails_and_names_the_setting_of_a_compose_file_below() {
        let dir = tempfile::tempdir().unwrap();
        let worktree = dir.path();
        std::fs::create_dir_all(worktree.join("apps/api")).unwrap();
        std::fs::create_dir_all(worktree.join("node_modules/pkg")).unwrap();
        std::fs::write(worktree.join("node_modules/pkg/compose.yaml"), "services: {}\n").unwrap();

        // `docker compose` may find its file through a `COMPOSE_FILE` doctor does not read.
        let none = compose_file_check(worktree, &loaded(worktree, COMPOSE_SERVICES));
        assert_eq!(none.status, CheckStatus::Skipped, "{}", none.message);
        assert_eq!(none.fix, None);
        assert!(
            !none.message.contains("compose_file = ") && !none.message.contains("node_modules"),
            "{}",
            none.message
        );

        std::fs::write(worktree.join("apps/api/docker-compose.yml"), "services: {}\n").unwrap();
        let below = compose_file_check(worktree, &loaded(worktree, COMPOSE_SERVICES));
        assert_eq!(below.status, CheckStatus::Skipped, "{}", below.message);
        assert!(
            below
                .message
                .contains("declare apps/api/docker-compose.yml with `compose_file = \"apps/api/docker-compose.yml\"` in the [settings] of procly.toml"),
            "{}",
            below.message
        );

        std::fs::write(worktree.join("compose.yaml"), "services: {}\n").unwrap();
        let rooted = compose_file_check(worktree, &loaded(worktree, COMPOSE_SERVICES));
        assert_eq!(rooted.status, CheckStatus::Skipped, "{}", rooted.message);
        assert!(!rooted.message.contains("compose_file = "), "{}", rooted.message);
    }

    #[test]
    fn without_a_compose_service_the_check_is_skipped_and_names_the_setting_of_a_compose_file_below() {
        let dir = tempfile::tempdir().unwrap();
        let worktree = dir.path();
        let processes = "[services.web]\nrun = 'npm run dev'\n";
        let nothing = compose_file_check(worktree, &loaded(worktree, processes));
        assert_eq!(nothing.status, CheckStatus::Skipped);
        assert!(!nothing.message.contains("compose_file = "), "{}", nothing.message);

        std::fs::create_dir_all(worktree.join("infra")).unwrap();
        std::fs::write(worktree.join("infra/compose.yml"), "services: {}\n").unwrap();
        let below = compose_file_check(worktree, &loaded(worktree, processes));
        assert_eq!(below.status, CheckStatus::Skipped);
        assert!(
            below.message.contains("`compose_file = \"infra/compose.yml\"`"),
            "{}",
            below.message
        );
    }
}
