mod client;
mod config;
mod daemon;
mod derived;
mod driver;
mod duration;
mod envfile;
mod guard;
mod init;
mod json;
mod output;
mod placeholder;
mod ports;
mod proposal;
mod protocol;
mod state_dir;
mod status;
mod topology;
mod unit;

use std::fmt::Write as _;
use std::io::{Read as _, Write as _};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::Context as _;
use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::protocol::{
    BlockOutcome, BlockReport, Conflict, EjectReport, Held, InitReport, JobEnd, Occupant, Released, Request, Response,
    ServiceEnd, UpService, Written,
};
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
/// The exit code of `up` for a Compose service whose Leases or container name are held by what runs outside Procly.
const EXIT_CONFLICT: u8 = 5;
/// The exit code of a command that failed in Procly itself, the one `timeout(1)` uses for its own failures.
const EXIT_PROCLY_FAILURE: u8 = 125;

/// The flag that turns the text of a command into one JSON document.
#[derive(Debug, Clone, Copy, Args)]
struct JsonFlag {
    /// Print one JSON document carrying a `schema_version` on stdout instead of text. A command that fails still
    /// reports on stderr and exits with the same code. See docs/json.md.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Parser)]
#[command(
    name = "procly",
    version,
    about,
    after_help = "Exit codes: 0 on success, 2 on a usage error, 3 when `up` finds the Service ended before it was \
                  healthy, 4 when `up` finds a Port mismatch, 5 when `up` finds a conflict with what runs outside Procly, 124 when `wait` finds the Job timed out or `up` \
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
        /// Kill the Job's whole process tree once it has run this long, as a number followed by `ms`, `s`, `m`, `h` or `d`
        /// (`500ms`, `90s`, `5m`, `1h`), a bare number meaning seconds. `procly wait` then exits with 124.
        #[arg(long, default_value = "30m", value_parser = duration::parse)]
        timeout: Duration,
        #[command(flatten)]
        json: JsonFlag,
        /// The command, run by `/bin/sh -c`.
        command: String,
    },
    /// Start a declared Service of the current Worktree, or find it running, wait until it is healthy and print
    /// its name, `started` or `reused`, its URL and its port on one line. A Service covered by a Group that runs is
    /// `reused (via <group>)`. Without a Service, start every declared Service no Group covers, one line each.
    #[command(
        after_help = "Exit codes: 0 when the Service is healthy, 3 when it stopped before it became healthy \
                            (it exited, was killed by a signal, was stopped by `procly down` or for being idle, or kept crashing \
                            until Procly gave up restarting it), 4 when it listened on another port than its Lease \
                            (Port mismatch) and its process tree was stopped, 5 when what it needs is held outside \
                            Procly (a conflict, with the command that stops it, which Procly never runs), 124 when it \
                            did not become healthy \
                            within its health timeout and its process tree was stopped, 125 when Procly itself \
                            fails. See docs/exit-codes.md."
    )]
    Up {
        /// Exclude the files Procly generates through the `.gitignore` of the Worktree, as a Managed block, instead
        /// of the git exclude file shared by the Repository.
        #[arg(long)]
        gitignore: bool,
        #[command(flatten)]
        json: JsonFlag,
        service: Option<String>,
    },
    /// Print the variables Procly injects into a Service of the current Worktree as shell exports: its Leases, its
    /// Derived variables, `PROCLY=1` and `PROCLY_SERVICE`. Without starting it, nor the environment of the caller.
    #[command(
        after_help = "Run a one-off command with the environment of a Service: `eval \"$(procly env web)\"`. \
                      Exit codes: 0 on success, 125 when Procly itself fails, such as for a Service that is not \
                      declared or a Derived variable that cannot be rendered. See docs/exit-codes.md."
    )]
    Env {
        #[command(flatten)]
        json: JsonFlag,
        service: String,
    },
    /// Stop a Service of the current Worktree, or all of its Services without an argument, with their whole
    /// process trees. The Daemon logs who asked. A Compose service is let go of instead: its container keeps
    /// running while another Worktree needs it, then stops after its Grace period.
    Down {
        #[command(flatten)]
        json: JsonFlag,
        service: Option<String>,
    },
    /// Wait for a Job to end, print its output and exit with its exit code (124 on timeout).
    Wait {
        #[command(flatten)]
        json: JsonFlag,
        job: String,
    },
    /// Print the recent output of a running or ended Job, or of a Service of the current Worktree.
    #[command(
        after_help = "A Job identifier printed by `procly run` names that Job, any other name a Service of the current \
                      Worktree, whose output is kept across a restart of the Daemon. The output of a Compose service \
                      is the last 1000 lines of its container, read with `docker compose logs`. Exit codes: 0 on \
                      success, 125 when Procly itself fails, such as for a name that is neither a Job nor a Service. \
                      See docs/exit-codes.md."
    )]
    Logs {
        #[command(flatten)]
        json: JsonFlag,
        job_or_service: String,
    },
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
    /// Steer coding agents to Procly: write the Rules block into the instruction file of the current Worktree, and the
    /// Guard into its Claude Code settings. Nothing is written unless you run this.
    Agent {
        #[command(subcommand)]
        command: AgentCommand,
    },
    /// Make the Guards and Rules blocks of this Repository stand aside, so that its agents start dev servers and
    /// containers as they did before Procly. Nothing is removed, `procly on` brings them back.
    Off {
        #[command(flatten)]
        json: JsonFlag,
    },
    /// Bring back the Guards and Rules blocks of this Repository after `procly off`.
    On {
        #[command(flatten)]
        json: JsonFlag,
    },
    /// Remove everything Procly wrote into this Repository: stop its Services, then take out the Rules blocks, the
    /// Guard, the exclusion lines and the Env files that nobody edited, and print what was removed and what was left.
    /// Docker volumes and Logical resources are kept. A Managed block you edited is left as it is, and a prompt
    /// describing what to undo in it is printed.
    #[command(
        after_help = "Exit codes: 0 when the Repository is ejected, also when blocks were left because you edited them, \
                      125 when Procly itself fails, such as for a container that cannot be stopped or the agent \
                      CLI of `--agent` failing, in which case what eject removed stays removed. See docs/eject.md and \
                      docs/exit-codes.md."
    )]
    Eject {
        /// Hand the prompt to your own coding agent in its non-interactive mode, from the current directory, instead
        /// of printing it. Without this flag no agent is ever run. With `--json`, the standard output of the agent
        /// goes to stderr.
        #[arg(long, value_enum)]
        agent: Option<AgentCli>,
        #[command(flatten)]
        json: JsonFlag,
    },
    /// The Claude Code hook that blocks the dev-server and compose commands Procly runs instead, and answers with
    /// the Procly command to run. It reads the tool call on its standard input and always exits 0.
    #[command(
        after_help = "Registered by `procly agent install` as a PreToolUse hook. A command it does not know is let \
                      through without asking the Daemon, and so is every command while the Repository is off or when \
                      the Daemon cannot answer. See docs/agents.md."
    )]
    Guard,
    /// Propose a `procly.toml` for this Worktree from the `dev` scripts of its `package.json` files and its compose
    /// file, print it on stdout and write it to the root of the Worktree. A `procly.toml` that exists is never
    /// overwritten.
    #[command(
        after_help = "Exit codes: 0 when the proposal is printed, also when a procly.toml existed and was left as it \
                      is, 125 when Procly itself fails, such as for a Worktree with no dev script and no compose \
                      file, a package.json or compose file that cannot be read, or a directory that is not in a \
                      git Repository. See docs/init.md and docs/exit-codes.md."
    )]
    Init {
        #[command(flatten)]
        json: JsonFlag,
    },
    /// Run the Daemon in the foreground. Every other command starts it on demand.
    #[command(hide = true)]
    Daemon,
    /// Append the standard input to the output file of a Service, keeping it bounded. The Daemon starts it.
    #[command(hide = true)]
    OutputFile { path: PathBuf },
}

/// A coding agent CLI that `eject --agent` can hand a prompt to, with the arguments that make it answer the prompt
/// once and exit.
#[derive(Debug, Clone, Copy, ValueEnum)]
enum AgentCli {
    /// `claude -p <prompt>`
    Claude,
    /// `codex exec <prompt>`
    Codex,
    /// `gemini -p <prompt>`
    Gemini,
    /// `opencode run <prompt>`
    Opencode,
}

impl AgentCli {
    fn program(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Gemini => "gemini",
            Self::Opencode => "opencode",
        }
    }

    /// The arguments before the prompt.
    fn non_interactive(self) -> &'static [&'static str] {
        match self {
            Self::Claude | Self::Gemini => &["-p"],
            Self::Codex => &["exec"],
            Self::Opencode => &["run"],
        }
    }
}

#[derive(Debug, Subcommand)]
enum AgentCommand {
    /// Write the Rules block into `AGENTS.md` (or `CLAUDE.md` when that is the only instruction file) and the Guard
    /// into `.claude/settings.json`, both inside the current Worktree. Run it again to update them, a block you
    /// edited is left as it is.
    #[command(
        after_help = "Exit codes: 0 when the blocks are written, also when they already were or were left as they are \
                      because you edited them, 125 when Procly itself fails, such as for a settings file that is not \
                      JSON or an instruction file outside the Worktree. See docs/agents.md and docs/exit-codes.md."
    )]
    Install {
        /// The instruction file for the Rules block, inside the Worktree. Defaults to `AGENTS.md`, then `CLAUDE.md`.
        #[arg(long)]
        file: Option<PathBuf>,
        #[command(flatten)]
        json: JsonFlag,
    },
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
        #[command(flatten)]
        json: JsonFlag,
        /// The Worktree to prepare, or any directory in it. Defaults to the current directory.
        path: Option<PathBuf>,
    },
}

#[derive(Debug, Subcommand)]
enum UnitCommand {
    /// Write the Session unit and have the session manager start the Daemon now and with every session, restarting
    /// it if it crashes. Run it again after moving the procly executable or the state directory.
    Install {
        #[command(flatten)]
        json: JsonFlag,
    },
    /// Print where the Session unit is, whether it is installed, whether it starts the Daemon with the session and
    /// whether that Daemon runs now.
    Inspect {
        #[command(flatten)]
        json: JsonFlag,
    },
    /// Stop the Daemon the Session unit started, with its Services, and remove the unit. The next command starts a
    /// Daemon on demand again.
    Remove {
        #[command(flatten)]
        json: JsonFlag,
    },
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
    // A hook that fails must not stop the agent, so the Guard answers for itself.
    if let Command::Guard = command {
        return Ok(guard_hook());
    }
    // The writer of an output file outlives the Daemon, it needs nothing from the state directory.
    if let Command::OutputFile { path } = &command {
        daemon::write_output(path)?;
        return Ok(ExitCode::SUCCESS);
    }
    // Init reads the Worktree only, so it never starts the Daemon and needs nothing from the state directory.
    if let Command::Init { json } = &command {
        return init(json.json);
    }
    let state = StateDir::resolve()?;
    match command {
        Command::Run { timeout, json, command } => {
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
                Response::Run { job, outcome } if json.json => json::print(&json::Run { job: &job, outcome })?,
                Response::Run { job, outcome } => {
                    println!("{job}");
                    eprintln!("{}", outcome.as_str());
                }
                other => return unexpected(other),
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Up {
            gitignore,
            json,
            service,
        } => up(&state, service, gitignore, json.json),
        Command::Env { json, service } => env(&state, &service, json.json),
        Command::Down { json, service } => down(&state, service, json.json),
        Command::Wait { json, job } => match client::send(&state, &Request::Wait { job: job.clone() })? {
            Response::Ended { output, end } => {
                if json.json {
                    json::print(&json::Wait {
                        job: &job,
                        exit_code: job_exit_code(end),
                        end,
                        output: &output,
                    })?;
                } else {
                    print(&output)?;
                }
                Ok(exit_code(&job, end))
            }
            other => unexpected(other),
        },
        Command::Logs { json, job_or_service } => logs(&state, &job_or_service, json.json),
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
            command: WorktreeCommand::Init { json, path },
        } => worktree_init(&state, path, json.json),
        Command::Unit { command } => unit(&state, &command),
        Command::Agent {
            command: AgentCommand::Install { file, json },
        } => agent_install(&state, file, json.json),
        Command::Off { json } => switch(&state, true, json.json),
        Command::On { json } => switch(&state, false, json.json),
        Command::Eject { agent, json } => eject(&state, agent, json.json),
        Command::Init { .. } => unreachable!("init is answered before the state directory is resolved"),
        Command::Guard => unreachable!("the Guard is answered before the state directory is resolved"),
        Command::OutputFile { .. } => unreachable!("the output file is written before the state directory is resolved"),
        Command::Daemon => {
            daemon::run(&state)?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// Prints the recent output of the Job or the Service `job_or_service`.
fn logs(state: &StateDir, job_or_service: &str, json: bool) -> anyhow::Result<ExitCode> {
    let cwd = std::env::current_dir().context("cannot read the current directory")?;
    let request = Request::Logs {
        name: job_or_service.to_owned(),
        cwd,
        env: caller_env(),
    };
    match client::send(state, &request)? {
        Response::Logs { output } => {
            if json {
                json::print(&json::Logs {
                    name: job_or_service,
                    output: &output,
                })?;
            } else {
                print(&output)?;
            }
            Ok(ExitCode::SUCCESS)
        }
        other => unexpected(other),
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
fn up(state: &StateDir, service: Option<String>, gitignore: bool, json: bool) -> anyhow::Result<ExitCode> {
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
            via,
            port,
            url,
            ports,
            needs,
        } => {
            let requested = UpService {
                service,
                outcome,
                via,
                port,
                url,
                ports,
            };
            up_reached(&requested, &needs, json)
        }
        Response::UpAll { services } => up_all(&services, json),
        Response::Unhealthy {
            service,
            end,
            output,
            needed_by,
            ..
        } => up_unhealthy(service, end, &output, needed_by, json),
        Response::Conflict {
            service,
            conflicts,
            needed_by,
        } => {
            if json {
                json::print(&json::Up::Conflict {
                    service: &service,
                    needed_by: needed_by.as_deref(),
                    conflicts: &conflicts,
                })?;
            }
            Ok(conflicted(service, &conflicts, needed_by))
        }
        other => unexpected(other),
    }
}

/// Prints the Service `up` was asked for, healthy, after the `needs` it brought up first.
fn up_reached(requested: &UpService, needs: &[UpService], json: bool) -> anyhow::Result<ExitCode> {
    if json {
        let services = needs
            .iter()
            .map(|need| json::Reached::new(need, Some(&requested.service)))
            .chain([json::Reached::new(requested, None)])
            .collect();
        json::print(&json::Up::Healthy { services })?;
        return Ok(ExitCode::SUCCESS);
    }
    // Stdout is the requested Service alone, what it needed is told on stderr.
    for need in needs {
        eprintln!("{} (needed by {})", reached_line(need), requested.service);
    }
    println!("{}", reached_line(requested));
    Ok(ExitCode::SUCCESS)
}

/// Prints every Service `up` brought up or found running, since every Service was asked for, in the order they came up.
fn up_all(services: &[UpService], json: bool) -> anyhow::Result<ExitCode> {
    if json {
        let services = services.iter().map(|up| json::Reached::new(up, None)).collect();
        json::print(&json::Up::Healthy { services })?;
        return Ok(ExitCode::SUCCESS);
    }
    for up in services {
        println!("{}", reached_line(up));
    }
    Ok(ExitCode::SUCCESS)
}

/// Prints why a Service is not healthy, on stderr and, with `json`, in a document on stdout.
fn up_unhealthy(
    service: String,
    end: ServiceEnd,
    output: &str,
    needed_by: Option<String>,
    json: bool,
) -> anyhow::Result<ExitCode> {
    if json {
        json::print(&json::Up::Unhealthy {
            service: &service,
            needed_by: needed_by.as_deref(),
            end,
            output: tail(output, OUTPUT_TAIL_LINES),
        })?;
    }
    let service = match needed_by {
        Some(needed_by) => format!("{service}, needed by {needed_by},"),
        None => service,
    };
    let (why, code) = unhealthy(end);
    if output.is_empty() {
        eprintln!("procly: service {service} {why}, it printed nothing");
    } else {
        eprintln!("procly: service {service} {why}, its recent output:");
        eprint!("{}", tail(output, OUTPUT_TAIL_LINES));
    }
    Ok(ExitCode::from(code))
}

/// Why a Service is not healthy, after its name, and the exit code of `up` for it.
fn unhealthy(end: ServiceEnd) -> (String, u8) {
    match end {
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
    }
}

/// Prints the conflicts that keep a Service from starting, one per line.
fn conflicted(service: String, conflicts: &[Conflict], needed_by: Option<String>) -> ExitCode {
    let service = match needed_by {
        Some(needed_by) => format!("{service}, needed by {needed_by},"),
        None => service,
    };
    eprintln!("procly: service {service} needs what runs outside Procly, which Procly never stops:");
    for conflict in conflicts {
        eprintln!("  {}", conflict_line(conflict));
    }
    ExitCode::from(EXIT_CONFLICT)
}

/// What holds what a Service needs, and how to stop it.
fn conflict_line(conflict: &Conflict) -> String {
    let occupant = match &conflict.occupant {
        Occupant::Container { name, .. } => format!("container {name}"),
        Occupant::Process { pid, command } => format!("process {pid} {command}"),
        Occupant::Unknown { .. } => "something Procly cannot name".to_owned(),
    };
    let what = match &conflict.held {
        Held::Port { port } => format!("port {port} of its Lease is held by {occupant} started outside Procly"),
        Held::ContainerName { name } => {
            format!("container name {name} is held by a container compose did not create for this service")
        }
        Held::Service { ports, leased } => {
            let service = match &conflict.occupant {
                Occupant::Container {
                    service: Some(service), ..
                } => format!("compose service {service}"),
                _ => "its compose service".to_owned(),
            };
            format!(
                "{service} already runs outside Procly in {occupant}, publishing {} instead of its {}",
                status::ports_text(ports),
                match leased.as_slice() {
                    [port] => format!("Lease, port {port}"),
                    leased => format!("Leases, {}", status::ports_text(leased)),
                }
            )
        }
    };
    match (&conflict.stop, &conflict.held, &conflict.occupant) {
        (Some(stop), Held::ContainerName { .. }, _) => format!("{what}, stop and remove it with `{stop}`"),
        (Some(stop), ..) => format!("{what}, stop it with `{stop}`"),
        (None, _, Occupant::Unknown { find }) => format!("{what}, find it with `{find}`"),
        (None, ..) => what,
    }
}

/// Runs `env`, printing one `export` line per variable.
fn env(state: &StateDir, service: &str, json: bool) -> anyhow::Result<ExitCode> {
    let cwd = std::env::current_dir().context("cannot read the current directory")?;
    match client::send(
        state,
        &Request::Env {
            service: service.to_owned(),
            cwd,
        },
    )? {
        Response::Env { variables } => {
            if json {
                json::print(&json::Env {
                    service,
                    variables: json::Variables(&variables),
                })?;
            } else {
                print(&envfile::exports(&variables))?;
            }
            Ok(ExitCode::SUCCESS)
        }
        other => unexpected(other),
    }
}

/// Where to reach a Service that is up: its name, `started` or `reused` and the Group it runs in when it is covered
/// by one, its URL and its port.
fn reached_line(up: &UpService) -> String {
    let service = &up.service;
    let outcome = match &up.via {
        Some(group) => format!("{} (via {group})", up.outcome.as_str()),
        None => up.outcome.as_str().to_owned(),
    };
    match (&up.url, up.port) {
        (Some(url), Some(port)) => format!("{service} {outcome} {url} port {port}"),
        _ => format!("{service} {outcome}"),
    }
}

/// Runs `down`, printing one line per Service.
fn down(state: &StateDir, service: Option<String>, json: bool) -> anyhow::Result<ExitCode> {
    let cwd = std::env::current_dir().context("cannot read the current directory")?;
    let request = Request::Down {
        service: service.clone(),
        cwd,
    };
    match client::send(state, &request)? {
        Response::Down { stopped, released } if json => {
            json::print(&json::Down {
                service: service.as_deref(),
                stopped: &stopped,
                released: &released,
            })?;
            Ok(ExitCode::SUCCESS)
        }
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
        UnitCommand::Install { json } => {
            unit.install()?;
            if json.json {
                json::print(&json::UnitInstalled {
                    path: &unit.path,
                    installed: true,
                })?;
            } else {
                println!("installed {path}");
            }
        }
        UnitCommand::Inspect { json } => {
            let inspection = unit.inspect()?;
            if json.json {
                json::print(&json::UnitInspection {
                    path: &unit.path,
                    installed: inspection.installed,
                    starts_with_session: inspection.enabled,
                    daemon_running: inspection.running,
                })?;
            } else {
                let yes_no = |yes| if yes { "yes" } else { "no" };
                println!("unit {path}");
                println!("installed {}", yes_no(inspection.installed));
                println!("starts with the session {}", yes_no(inspection.enabled));
                println!("daemon running {}", yes_no(inspection.running));
            }
        }
        UnitCommand::Remove { json } => {
            let removed = unit.remove()?;
            if json.json {
                json::print(&json::UnitRemoved {
                    path: &unit.path,
                    removed,
                })?;
            } else if removed {
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

/// Runs `agent install`, printing what became of the Rules block and of the Guard.
fn agent_install(state: &StateDir, file: Option<PathBuf>, json: bool) -> anyhow::Result<ExitCode> {
    let cwd = std::env::current_dir().context("cannot read the current directory")?;
    let file = file.map(|file| cwd.join(file));
    match client::send(state, &Request::AgentInstall { cwd, file })? {
        Response::AgentInstall(report) if json => {
            json::print(&report)?;
            Ok(ExitCode::SUCCESS)
        }
        Response::AgentInstall(report) => {
            print_block("rules", &report.rules);
            print_block("guard", &report.guard);
            if report.off {
                eprintln!("procly: this Repository is off, the Rules block says so, `procly on` turns it back on");
            }
            Ok(ExitCode::SUCCESS)
        }
        other => unexpected(other),
    }
}

/// Runs `off` or `on`, printing the new state and what became of the Rules blocks.
fn switch(state: &StateDir, off: bool, json: bool) -> anyhow::Result<ExitCode> {
    let cwd = std::env::current_dir().context("cannot read the current directory")?;
    match client::send(state, &Request::Switch { cwd, off })? {
        Response::Switched(report) if json => {
            json::print(&report)?;
            Ok(ExitCode::SUCCESS)
        }
        Response::Switched(report) => {
            println!("{}", if report.off { "off" } else { "on" });
            for block in &report.rules {
                print_block("rules", block);
            }
            Ok(ExitCode::SUCCESS)
        }
        other => unexpected(other),
    }
}

/// Runs `init`, printing the proposal on stdout and on stderr whether it was written.
fn init(json: bool) -> anyhow::Result<ExitCode> {
    let cwd = std::env::current_dir().context("cannot read the current directory")?;
    let report = init::propose(&cwd)?;
    if json {
        json::print(&report)?;
        return Ok(ExitCode::SUCCESS);
    }
    print(&report.toml)?;
    if report.written {
        eprintln!("procly: wrote {}", report.file.display());
    } else {
        eprintln!(
            "procly: {} exists already, left as it is, the proposal above is not written",
            report.file.display()
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// Runs `eject`, printing what was stopped, removed and left, then the prompt for what was left, which `--agent` hands
/// to the coding agent instead.
fn eject(state: &StateDir, agent: Option<AgentCli>, json: bool) -> anyhow::Result<ExitCode> {
    let cwd = std::env::current_dir().context("cannot read the current directory")?;
    let report = match client::send(state, &Request::Eject { cwd })? {
        Response::Eject(report) => report,
        other => return unexpected(other),
    };
    // The document comes before the agent runs, so that it exists even when the agent fails.
    if json {
        let handed_to = agent.filter(|_| report.prompt.is_some());
        json::print(&json::Eject {
            stopped: &report.stopped,
            removed: &report.removed,
            left: &report.left,
            prompt: report.prompt.as_deref().filter(|_| handed_to.is_none()),
            agent: handed_to.map(AgentCli::program),
        })?;
    } else {
        print(&eject_summary(&report))?;
    }
    let Some(prompt) = &report.prompt else {
        if let (Some(agent), false) = (agent, json) {
            eprintln!(
                "procly: nothing was left, so there is no prompt to hand to {}",
                agent.program()
            );
        }
        return Ok(ExitCode::SUCCESS);
    };
    match agent {
        None => {
            if !json {
                eprintln!(
                    "procly: {} edited, left as it is, the prompt below describes what to undo, `procly eject \
                     --agent <cli>` hands it to your coding agent",
                    items_were(report.left.len())
                );
                print(&format!("\n{prompt}"))?;
            }
            Ok(ExitCode::SUCCESS)
        }
        Some(agent) => {
            let mut command = std::process::Command::new(agent.program());
            command.args(agent.non_interactive()).arg(prompt);
            if json {
                // Stdout holds the document alone.
                command.stdout(std::io::stderr());
            } else {
                eprintln!("procly: handing the prompt to {}", agent.program());
            }
            let status = command
                .status()
                .with_context(|| format!("cannot run {}, is it installed and on the PATH?", agent.program()))?;
            if status.success() {
                Ok(ExitCode::SUCCESS)
            } else {
                anyhow::bail!(
                    "{} exited with {status}, what it was to undo is left as it is",
                    agent.program()
                )
            }
        }
    }
}

/// `1 item was` or `3 items were`, for the items eject left.
fn items_were(count: usize) -> String {
    match count {
        1 => "1 item was".to_owned(),
        _ => format!("{count} items were"),
    }
}

/// One line per Service stopped, per file removed and per file left, then what eject never touches.
fn eject_summary(report: &EjectReport) -> String {
    let mut text = String::new();
    for stopped in &report.stopped {
        let _ = match &stopped.worktree {
            Some(worktree) => writeln!(text, "stopped {} of {}", stopped.service, worktree.display()),
            None => writeln!(
                text,
                "stopped {} (Compose service, its volumes are kept)",
                stopped.service
            ),
        };
    }
    for removed in &report.removed {
        let deleted = if removed.deleted && removed.kind != Written::EnvFile {
            ", the file held nothing else and was deleted"
        } else {
            ""
        };
        let _ = writeln!(
            text,
            "removed {} {}{deleted}",
            removed.kind.as_str(),
            removed.file.display()
        );
    }
    for left in &report.left {
        let _ = writeln!(text, "left {} {}, edited", left.kind.as_str(), left.file.display());
    }
    text.push_str("left Docker volumes and Logical resources\n");
    text.push_str("left the files copied by worktree init and the state of the Daemon, Leases and off\n");
    text
}

/// Runs the Guard for a Claude Code `PreToolUse` hook: reads the tool call on stdin and, for a command it knows while
/// the Repository is not off, denies it with the Procly command to run. Whatever else happens, the command is let
/// through, since a Guard that breaks must not break the agent.
fn guard_hook() -> ExitCode {
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        return ExitCode::SUCCESS;
    }
    let Some(call) = guard::tool_call(&input) else {
        return ExitCode::SUCCESS;
    };
    let Some(recognized) = guard::recognize(&call.command) else {
        return ExitCode::SUCCESS;
    };
    let asked = StateDir::resolve().map_err(anyhow::Error::from).and_then(|state| {
        let cwd = match call.cwd {
            Some(cwd) => cwd,
            None => std::env::current_dir().context("cannot read the current directory")?,
        };
        Ok(client::send(&state, &Request::Guard { cwd })?)
    });
    match asked {
        Ok(Response::Guard {
            off: false,
            at_root,
            services,
        }) => println!("{}", guard::denial(&recognized, &services, at_root)),
        Ok(Response::Guard { off: true, .. }) => {}
        Ok(Response::Error { message }) => eprintln!("procly guard: {message}, letting the command through"),
        Ok(other) => eprintln!("procly guard: unexpected answer {other:?}, letting the command through"),
        Err(error) => eprintln!("procly guard: {error:#}, letting the command through"),
    }
    ExitCode::SUCCESS
}

/// One line on stdout for a Managed block, and on stderr why one the user edited was left as it is.
fn print_block(name: &str, block: &BlockReport) {
    println!("{name} {} {}", block.file.display(), block.outcome.as_str());
    if block.outcome == BlockOutcome::Edited {
        eprintln!(
            "procly: the {name} block in {} was edited, left as it is",
            block.file.display()
        );
    }
}

/// Runs `worktree init`, printing one line per file copied and per setup command run.
fn worktree_init(state: &StateDir, path: Option<PathBuf>, json: bool) -> anyhow::Result<ExitCode> {
    let cwd = std::env::current_dir().context("cannot read the current directory")?;
    let path = path.map_or(cwd.clone(), |path| cwd.join(path));
    let request = Request::WorktreeInit {
        path,
        env: caller_env(),
    };
    match client::send_with_progress(state, &request, print_setup_progress)? {
        Response::WorktreeInit(report) if json => {
            json::print(&json::WorktreeInit::Ready { report: &report })?;
            Ok(ExitCode::SUCCESS)
        }
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
            if json {
                json::print(&json::WorktreeInit::SetupFailed {
                    report: &report,
                    command: &command,
                    end,
                    output: tail(&output, OUTPUT_TAIL_LINES),
                })?;
            } else {
                print_init(&report);
            }
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

/// One line on stderr for a setup command still running, so that a long setup shows it is alive. Stdout, and with it the
/// JSON document, stays as it is.
fn print_setup_progress(progress: &Response) {
    if let Response::SetupProgress {
        command,
        elapsed_ms,
        log,
    } = progress
    {
        let elapsed = duration::format(Duration::from_secs(elapsed_ms / 1000));
        eprintln!(
            "procly: setup command `{command}` still running after {elapsed}, log: {}",
            log.display()
        );
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

/// The exit code of `wait` for a Job that ended as `end`.
fn job_exit_code(end: JobEnd) -> u8 {
    match end {
        JobEnd::Exited { code } => u8::try_from(code).unwrap_or(EXIT_PROCLY_FAILURE),
        // The shell convention, so `wait` exits as the command would have in a terminal.
        JobEnd::Signaled { signal } => u8::try_from(128 + signal).unwrap_or(EXIT_PROCLY_FAILURE),
        JobEnd::TimedOut { .. } => EXIT_TIMEOUT,
    }
}

fn exit_code(job: &str, end: JobEnd) -> ExitCode {
    if let JobEnd::TimedOut { timeout_ms } = end {
        let timeout = duration::format(Duration::from_millis(timeout_ms));
        eprintln!("procly: job {job} timed out after {timeout}, its process tree was killed");
    }
    ExitCode::from(job_exit_code(end))
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
