mod client;
mod config;
mod daemon;
mod derived;
mod duration;
mod envfile;
mod output;
mod ports;
mod protocol;
mod state_dir;
mod status;
mod topology;
mod unit;

use std::io::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::Context as _;
use clap::{Parser, Subcommand};

use crate::protocol::{InitReport, JobEnd, Released, Request, Response, ServiceEnd, StartOutcome};
use crate::state_dir::StateDir;

/// The exit code of `wait` for a Job killed on timeout, and of `up` for a Service never healthy, the one
/// `timeout(1)` uses.
const EXIT_TIMEOUT: u8 = 124;
/// How many lines of output `up` prints when a Service fails to become healthy.
const OUTPUT_TAIL_LINES: usize = 20;
/// The exit code of `up` for a Service that ended before it became healthy.
const EXIT_SERVICE_ENDED: u8 = 3;
/// The exit code of `up` for a Service whose process listened on another port than its Lease.
const EXIT_PORT_MISMATCH: u8 = 4;
/// The exit code of a command that failed in Procly itself, the one `timeout(1)` uses for its own failures.
const EXIT_PROCLY_FAILURE: u8 = 125;

#[derive(Debug, Parser)]
#[command(
    name = "procly",
    version,
    about,
    after_help = "Exit codes: 0 on success, 2 on a usage error, 3 when `up` finds the Service ended before it was \
                  healthy, 4 when `up` finds a Port mismatch, 124 when `wait` finds the Job timed out or `up` \
                  waited past the health timeout, 125 when Procly itself fails, otherwise `wait` exits with the Job's own code (128 + N for a Job killed by \
                  signal N) and `worktree init` with the code of the setup command that failed. See \
                  docs/exit-codes.md."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Start a Job running a shell command in the current directory, print its identifier on stdout, and on stderr
    /// `started`, or `reused` when the same command already runs in the same Worktree and directory.
    Run {
        /// Kill the Job's whole process tree once it has run this long (`90s`, `5m`, `1h`).
        #[arg(long, default_value = "30m", value_parser = duration::parse)]
        timeout: Duration,
        /// The command, run by `/bin/sh -c`.
        command: String,
    },
    /// Start a declared Service of the current Worktree, or find it running, wait until it is healthy and print
    /// its name, `started` or `reused`, its URL and its port on one line.
    #[command(
        after_help = "Exit codes: 0 when the Service is healthy, 3 when it stopped before it became healthy \
                            (it exited, was killed by a signal, was stopped by `procly down` or for being idle, or kept crashing \
                            until Procly gave up restarting it), 4 when it listened on another port than its Lease \
                            (Port mismatch) and its process tree was stopped, 124 when it did not become healthy \
                            within its health timeout and its process tree was stopped, 125 when Procly itself \
                            fails. See docs/exit-codes.md."
    )]
    Up {
        /// Exclude the files Procly generates through the `.gitignore` of the Worktree, as a Managed block, instead
        /// of the git exclude file shared by the Repository.
        #[arg(long)]
        gitignore: bool,
        service: String,
    },
    /// Print the variables Procly injects into a Service of the current Worktree as shell exports: its Leases, its
    /// Derived variables, `PROCLY=1` and `PROCLY_SERVICE`. Without starting it, nor the environment of the caller.
    #[command(
        after_help = "Run a one-off command with the environment of a Service: `eval \"$(procly env web)\"`. \
                      Exit codes: 0 on success, 125 when Procly itself fails, such as for a Service that is not \
                      declared or a Derived variable that cannot be rendered. See docs/exit-codes.md."
    )]
    Env { service: String },
    /// Stop a Service of the current Worktree, or all of its Services without an argument, with their whole
    /// process trees. The Daemon logs who asked. A Compose service is let go of instead: its container keeps
    /// running while another Worktree needs it, then stops after its Grace period.
    Down { service: Option<String> },
    /// Wait for a Job to end, print its output and exit with its exit code (124 on timeout).
    Wait { job: String },
    /// Print the recent output of a running or ended Job.
    Logs { job: String },
    /// Print the Repository, the current Worktree and its Jobs.
    Status {
        /// Report every Worktree of every Repository the Daemon knows, not only the current one.
        #[arg(long)]
        all: bool,
        /// Print JSON carrying a `schema_version`, instead of compact text.
        #[arg(long)]
        json: bool,
    },
    /// Prepare a Worktree.
    Worktree {
        #[command(subcommand)]
        command: WorktreeCommand,
    },
    /// Manage the Session unit, which starts the Daemon with your session: a launchd agent on macOS, a systemd user
    /// unit on Linux. Without it, the first command of a session starts the Daemon on demand.
    Unit {
        #[command(subcommand)]
        command: UnitCommand,
    },
    /// Run the Daemon in the foreground. Every other command starts it on demand.
    #[command(hide = true)]
    Daemon,
}

#[derive(Debug, Subcommand)]
enum WorktreeCommand {
    /// Make a fresh Worktree ready: copy the untracked files listed in `[worktree] copy` from the Main worktree
    /// when the Worktree lacks them, then run the `[worktree] setup` commands that have not succeeded yet.
    #[command(
        after_help = "Exit codes: 0 when the Worktree is ready, also when there was nothing to do, the exit code of \
                      the first setup command that failed (128 + N when killed by signal N, 124 past its time limit), \
                      which runs again on the next init, 125 when Procly itself fails. See docs/exit-codes.md."
    )]
    Init {
        /// The Worktree to prepare, or any directory in it. Defaults to the current directory.
        path: Option<PathBuf>,
    },
}

#[derive(Debug, Subcommand)]
enum UnitCommand {
    /// Write the Session unit and have the session manager start the Daemon now and with every session, restarting
    /// it if it crashes. Run it again after moving the procly executable or the state directory.
    Install,
    /// Print where the Session unit is, whether it is installed, whether it starts the Daemon with the session and
    /// whether that Daemon runs now.
    Inspect,
    /// Stop the Daemon the Session unit started, with its Services, and remove the unit. The next command starts a
    /// Daemon on demand again.
    Remove,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match execute(cli.command) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("procly: {error:#}");
            ExitCode::from(EXIT_PROCLY_FAILURE)
        }
    }
}

fn execute(command: Command) -> anyhow::Result<ExitCode> {
    let state = StateDir::resolve()?;
    match command {
        Command::Run { timeout, command } => {
            let cwd = std::env::current_dir().context("cannot read the current directory")?;
            let timeout_ms = u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX);
            let request = Request::Run {
                command,
                cwd,
                env: caller_env(),
                timeout_ms,
            };
            match client::send(&state, &request)? {
                // The identifier alone on stdout, so that `procly wait "$(procly run …)"` works.
                Response::Run { job, outcome } => {
                    println!("{job}");
                    eprintln!("{}", outcome.as_str());
                }
                other => return unexpected(other),
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Up { gitignore, service } => up(&state, service, gitignore),
        Command::Env { service } => env(&state, service),
        Command::Down { service } => down(&state, service),
        Command::Wait { job } => match client::send(&state, &Request::Wait { job: job.clone() })? {
            Response::Ended { output, end } => {
                print(&output)?;
                Ok(exit_code(&job, end))
            }
            other => unexpected(other),
        },
        Command::Logs { job } => match client::send(&state, &Request::Logs { job })? {
            Response::Logs { output } => {
                print(&output)?;
                Ok(ExitCode::SUCCESS)
            }
            other => unexpected(other),
        },
        Command::Status { all, json } => {
            let cwd = std::env::current_dir().context("cannot read the current directory")?;
            match client::send(&state, &Request::Status { cwd, all })? {
                Response::Status(report) => {
                    print(&if json {
                        status::json(&report)
                    } else {
                        status::text(&report)
                    })?;
                    Ok(ExitCode::SUCCESS)
                }
                other => unexpected(other),
            }
        }
        Command::Worktree {
            command: WorktreeCommand::Init { path },
        } => worktree_init(&state, path),
        Command::Unit { command } => unit(&state, &command),
        Command::Daemon => {
            daemon::run(&state)?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// The environment of this command, handed to the Job or Service it starts. Variables that are not UTF-8 are left out.
fn caller_env() -> Vec<(String, String)> {
    std::env::vars_os()
        .filter_map(|(name, value)| Some((name.into_string().ok()?, value.into_string().ok()?)))
        .collect()
}

/// The last `lines` lines of `output`.
fn tail(output: &str, lines: usize) -> &str {
    let start = output
        .trim_end_matches('\n')
        .rmatch_indices('\n')
        .nth(lines.saturating_sub(1))
        .map_or(0, |(newline, _)| newline + 1);
    &output[start..]
}

/// Runs `up`, printing where to reach the healthy Service, or why it is not.
fn up(state: &StateDir, service: String, gitignore: bool) -> anyhow::Result<ExitCode> {
    let cwd = std::env::current_dir().context("cannot read the current directory")?;
    let request = Request::Up {
        service,
        cwd,
        env: caller_env(),
        gitignore,
    };
    match client::send(state, &request)? {
        Response::Up {
            service,
            outcome,
            port,
            url,
            needs,
        } => {
            // Stdout is the requested Service alone, what it needed is told on stderr.
            for need in needs {
                eprintln!(
                    "{} (needed by {service})",
                    reached(&need.service, need.outcome, need.url, need.port)
                );
            }
            println!("{}", reached(&service, outcome, url, port));
            Ok(ExitCode::SUCCESS)
        }
        Response::Unhealthy {
            service,
            end,
            output,
            needed_by,
            ..
        } => {
            let service = match needed_by {
                Some(needed_by) => format!("{service}, needed by {needed_by},"),
                None => service,
            };
            let (why, code) = match end {
                ServiceEnd::Exited { code } => (
                    format!("exited with code {code} before it became healthy"),
                    EXIT_SERVICE_ENDED,
                ),
                ServiceEnd::Signaled { signal } => (
                    format!("was killed by signal {signal} before it became healthy"),
                    EXIT_SERVICE_ENDED,
                ),
                ServiceEnd::Stopped => (
                    "was stopped by procly down before it became healthy".to_owned(),
                    EXIT_SERVICE_ENDED,
                ),
                ServiceEnd::Idle { timeout_ms } => (
                    format!(
                        "was stopped after {} idle",
                        duration::format(Duration::from_millis(timeout_ms))
                    ),
                    EXIT_SERVICE_ENDED,
                ),
                ServiceEnd::Failed { restarts } => (
                    format!("kept crashing, it was restarted {restarts} times and is not restarted again"),
                    EXIT_SERVICE_ENDED,
                ),
                ServiceEnd::PortMismatch { leased, actual } => (
                    format!(
                        "listens on port {actual} instead of its Lease, port {leased} (Port mismatch), its process \
                         tree was stopped"
                    ),
                    EXIT_PORT_MISMATCH,
                ),
                // Ends a Daemon finds when it restarts, which `up` never waits on, since it starts the Service again.
                ServiceEnd::Lost | ServiceEnd::Orphaned { .. } | ServiceEnd::NotListening { .. } => (
                    "ended when the Daemon restarted, before it became healthy".to_owned(),
                    EXIT_SERVICE_ENDED,
                ),
                ServiceEnd::HealthTimeout { timeout_ms } => (
                    format!(
                        "did not become healthy within {}, its process tree was stopped",
                        duration::format(Duration::from_millis(timeout_ms))
                    ),
                    EXIT_TIMEOUT,
                ),
            };
            if output.is_empty() {
                eprintln!("procly: service {service} {why}, it printed nothing");
            } else {
                eprintln!("procly: service {service} {why}, its recent output:");
                eprint!("{}", tail(&output, OUTPUT_TAIL_LINES));
            }
            Ok(ExitCode::from(code))
        }
        other => unexpected(other),
    }
}

/// Runs `env`, printing one `export` line per variable.
fn env(state: &StateDir, service: String) -> anyhow::Result<ExitCode> {
    let cwd = std::env::current_dir().context("cannot read the current directory")?;
    match client::send(state, &Request::Env { service, cwd })? {
        Response::Env { variables } => {
            print(&envfile::exports(&variables))?;
            Ok(ExitCode::SUCCESS)
        }
        other => unexpected(other),
    }
}

/// Where to reach a Service that is up: its name, `started` or `reused`, its URL and its port.
fn reached(service: &str, outcome: StartOutcome, url: Option<String>, port: Option<u16>) -> String {
    match (url, port) {
        (Some(url), Some(port)) => format!("{service} {} {url} port {port}", outcome.as_str()),
        _ => format!("{service} {}", outcome.as_str()),
    }
}

/// Runs `down`, printing one line per Service.
fn down(state: &StateDir, service: Option<String>) -> anyhow::Result<ExitCode> {
    let cwd = std::env::current_dir().context("cannot read the current directory")?;
    let request = Request::Down {
        service: service.clone(),
        cwd,
    };
    match client::send(state, &request)? {
        Response::Down { stopped, released } => {
            match service {
                Some(service) if stopped.is_empty() && released.is_empty() => println!("{service} not running"),
                None if stopped.is_empty() && released.is_empty() => println!("no services running"),
                _ => {
                    for service in stopped {
                        println!("{service} stopped");
                    }
                    for released in &released {
                        println!("{}", let_go(released));
                    }
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        other => unexpected(other),
    }
}

/// Runs `unit install`, `unit inspect` or `unit remove`, without asking the Daemon, which the unit starts.
fn unit(state: &StateDir, command: &UnitCommand) -> anyhow::Result<ExitCode> {
    let unit = unit::Unit::resolve(state)?;
    let path = unit.path.display();
    match command {
        UnitCommand::Install => {
            unit.install()?;
            println!("installed {path}");
        }
        UnitCommand::Inspect => {
            let inspection = unit.inspect()?;
            let yes_no = |yes| if yes { "yes" } else { "no" };
            println!("unit {path}");
            println!("installed {}", yes_no(inspection.installed));
            println!("starts with the session {}", yes_no(inspection.enabled));
            println!("daemon running {}", yes_no(inspection.running));
        }
        UnitCommand::Remove => {
            if unit.remove()? {
                println!("removed {path}");
            } else {
                println!("not installed {path}");
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// What `down` tells about a Compose service the Worktree let go of: whether its container keeps running for others,
/// or when it stops.
fn let_go(released: &Released) -> String {
    let Released {
        service,
        others,
        grace_ms,
    } = released;
    match others {
        0 => format!(
            "{service} released, it stops after {} unless a Worktree needs it again",
            duration::format(Duration::from_millis(*grace_ms))
        ),
        1 => format!("{service} released, 1 other Worktree still needs it"),
        _ => format!("{service} released, {others} other Worktrees still need it"),
    }
}

/// Runs `worktree init`, printing one line per file copied and per setup command run.
fn worktree_init(state: &StateDir, path: Option<PathBuf>) -> anyhow::Result<ExitCode> {
    let cwd = std::env::current_dir().context("cannot read the current directory")?;
    let path = path.map_or(cwd.clone(), |path| cwd.join(path));
    let request = Request::WorktreeInit {
        path,
        env: caller_env(),
    };
    match client::send(state, &request)? {
        Response::WorktreeInit(report) => {
            if report.copied.is_empty() && report.ran.is_empty() {
                println!("nothing to do");
            }
            print_init(&report);
            Ok(ExitCode::SUCCESS)
        }
        Response::SetupFailed {
            report,
            command,
            end,
            output,
        } => {
            print_init(&report);
            let (why, code) = match end {
                JobEnd::Exited { code } => (
                    format!("exited with code {code}"),
                    u8::try_from(code).unwrap_or(EXIT_PROCLY_FAILURE),
                ),
                JobEnd::Signaled { signal } => (
                    format!("was killed by signal {signal}"),
                    u8::try_from(128 + signal).unwrap_or(EXIT_PROCLY_FAILURE),
                ),
                JobEnd::TimedOut { timeout_ms } => (
                    format!(
                        "ran past {}, its process tree was killed",
                        duration::format(Duration::from_millis(timeout_ms))
                    ),
                    EXIT_TIMEOUT,
                ),
            };
            let retry = "it will run again on the next worktree init";
            if output.is_empty() {
                eprintln!("procly: setup command `{command}` {why}, it printed nothing, {retry}");
            } else {
                eprintln!("procly: setup command `{command}` {why}, {retry}, its recent output:");
                eprint!("{}", tail(&output, OUTPUT_TAIL_LINES));
            }
            Ok(ExitCode::from(code))
        }
        other => unexpected(other),
    }
}

fn print_init(report: &InitReport) {
    for path in &report.missing {
        eprintln!("procly: {path} is not in the Main worktree, nothing to copy");
    }
    for path in &report.copied {
        println!("copied {path}");
    }
    for command in &report.ran {
        println!("ran {command}");
    }
}

fn exit_code(job: &str, end: JobEnd) -> ExitCode {
    match end {
        JobEnd::Exited { code } => ExitCode::from(u8::try_from(code).unwrap_or(EXIT_PROCLY_FAILURE)),
        // The shell convention, so `wait` exits as the command would have in a terminal.
        JobEnd::Signaled { signal } => ExitCode::from(u8::try_from(128 + signal).unwrap_or(EXIT_PROCLY_FAILURE)),
        JobEnd::TimedOut { timeout_ms } => {
            let timeout = duration::format(Duration::from_millis(timeout_ms));
            eprintln!("procly: job {job} timed out after {timeout}, its process tree was killed");
            ExitCode::from(EXIT_TIMEOUT)
        }
    }
}

fn print(output: &str) -> anyhow::Result<()> {
    let mut stdout = std::io::stdout().lock();
    stdout
        .write_all(output.as_bytes())
        .and_then(|()| stdout.flush())
        .context("cannot write to stdout")
}

fn unexpected(response: Response) -> anyhow::Result<ExitCode> {
    match response {
        Response::Error { message } => Err(anyhow::anyhow!(message)),
        other => Err(anyhow::anyhow!("unexpected answer from the daemon: {other:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tail_keeps_the_last_lines_with_their_newlines() {
        assert_eq!(tail("one\ntwo\nthree\n", 2), "two\nthree\n");
        assert_eq!(tail("one\ntwo\nthree", 2), "two\nthree");
    }

    #[test]
    fn a_released_container_tells_who_still_needs_it_or_when_it_stops() {
        let released = |others| Released {
            service: "db".to_owned(),
            others,
            grace_ms: 300_000,
        };
        assert_eq!(
            let_go(&released(0)),
            "db released, it stops after 5m unless a Worktree needs it again"
        );
        assert_eq!(let_go(&released(1)), "db released, 1 other Worktree still needs it");
        assert_eq!(let_go(&released(2)), "db released, 2 other Worktrees still need it");
    }

    #[test]
    fn the_tail_of_a_short_output_is_all_of_it() {
        assert_eq!(tail("one\ntwo\n", 20), "one\ntwo\n");
        assert_eq!(tail("", 20), "");
    }
}
