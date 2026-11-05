//! `procly logs --follow`: prints the output of a Job or a Service as the Daemon streams it, until it ends or Ctrl-C.

use std::io::Write as _;
use std::process::ExitCode;

use anyhow::Context as _;
use nix::sys::signal::{SigSet, Signal};

use crate::json::{self, FollowEvent};
use crate::protocol::{Request, Response};
use crate::state_dir::StateDir;
use crate::{caller_env, client, print, unexpected};

/// Follows the output of the Job or the Service `name`, as text or as one JSON document per line with `json`.
pub(crate) fn run(state: &StateDir, name: &str, json: bool) -> anyhow::Result<ExitCode> {
    let cwd = std::env::current_dir().context("cannot read the current directory")?;
    let request = Request::Logs {
        name: name.to_owned(),
        cwd,
        env: caller_env(),
        follow: true,
    };
    let responses = client::stream(state, &request)?;
    // Once connected, as a Daemon this command starts would inherit the blocked signal.
    exit_on_interrupt()?;
    for response in responses {
        match response? {
            Response::Logs { output } if json => print_event(name, FollowEvent::Backlog { output: &output })?,
            Response::Output { text } if json => print_event(name, FollowEvent::Output { output: &text })?,
            Response::Logs { output: text } | Response::Output { text } => print(&text)?,
            Response::OutputDropped { bytes } if json => print_event(name, FollowEvent::Dropped { bytes })?,
            Response::OutputDropped { bytes } => {
                eprintln!("procly: {bytes} bytes of output dropped, read too slowly");
            }
            Response::LogsEnded => {
                if json {
                    print_event(name, FollowEvent::Ended)?;
                }
                return Ok(ExitCode::SUCCESS);
            }
            other => return unexpected(other),
        }
    }
    anyhow::bail!("the daemon closed the connection before the output of {name} ended")
}

/// Prints one line of `logs --follow --json`.
fn print_event(name: &str, event: FollowEvent<'_>) -> anyhow::Result<()> {
    json::print(&json::Follow { name, event })
}

/// Makes Ctrl-C end this command with 0 between 2 writes of its output, so that a reader of `--json` never gets half a
/// line, and tells an interruption by the missing `ended` line rather than by the exit code.
fn exit_on_interrupt() -> anyhow::Result<()> {
    let mut interrupt = SigSet::empty();
    interrupt.add(Signal::SIGINT);
    // Blocked before the thread starts, which inherits the mask and is then the only one to take the signal.
    interrupt
        .thread_block()
        .context("cannot block SIGINT to handle Ctrl-C")?;
    std::thread::spawn(move || {
        if interrupt.wait().is_ok() {
            // Every line is written and flushed under this lock, so holding it means no line is half written.
            let mut stdout = std::io::stdout().lock();
            let _ = stdout.flush();
            std::process::exit(0);
        }
    });
    Ok(())
}
