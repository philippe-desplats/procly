mod client;
mod daemon;
mod duration;
mod output;
mod protocol;
mod state_dir;

use std::io::Write as _;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::Context as _;
use clap::{Parser, Subcommand};

use crate::protocol::{JobEnd, Request, Response};
use crate::state_dir::StateDir;

/// The exit code of `wait` for a Job killed on timeout, the one `timeout(1)` uses.
const EXIT_TIMEOUT: u8 = 124;
/// The exit code of a command that failed in Procly itself, the one `timeout(1)` uses for its own failures.
const EXIT_PROCLY_FAILURE: u8 = 125;

#[derive(Debug, Parser)]
#[command(
    name = "procly",
    version,
    about,
    after_help = "Exit codes: 0 on success, 124 when `wait` finds the Job timed out, 125 when Procly itself fails, \
                  otherwise `wait` exits with the Job's own code (128 + N for a Job killed by signal N)."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Start a Job running a shell command in the current directory, and print its identifier.
    Run {
        /// Kill the Job's whole process tree once it has run this long (`90s`, `5m`, `1h`).
        #[arg(long, default_value = "30m", value_parser = duration::parse)]
        timeout: Duration,
        /// The command, run by `/bin/sh -c`.
        command: String,
    },
    /// Wait for a Job to end, print its output and exit with its exit code (124 on timeout).
    Wait { job: String },
    /// Print the recent output of a running or ended Job.
    Logs { job: String },
    /// Run the Daemon in the foreground. Every other command starts it on demand.
    #[command(hide = true)]
    Daemon,
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
            let env = std::env::vars_os()
                .filter_map(|(name, value)| Some((name.into_string().ok()?, value.into_string().ok()?)));
            let timeout_ms = u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX);
            let request = Request::Run {
                command,
                cwd,
                env: env.collect(),
                timeout_ms,
            };
            match client::send(&state, &request)? {
                Response::Started { job } => println!("{job}"),
                other => return unexpected(other),
            }
            Ok(ExitCode::SUCCESS)
        }
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
        Command::Daemon => {
            daemon::run(&state)?;
            Ok(ExitCode::SUCCESS)
        }
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
