//! Says what held a port a test declared, when the Lease of its Service did not land on it.
//!
//! A test declares a port it got from [`super::free_port`]. When the Daemon finds that port taken as it grants the
//! Lease, the Lease goes to the Range and the test fails with a message that only shows the outcome. The diagnostic
//! here is added to that failure, and only to it: it looks, at the moment of the failure, at the processes listening
//! on the port and at the Leases of the Daemons the test started.
//!
//! Nothing here may fail or hang: every lookup is bounded in time and reports its own failure as text.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use nix::unistd::Pid;

/// How long a lookup may run, so that a stuck `lsof` or file system never holds up a failing suite.
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);
/// How often a running lookup is checked for its end.
const LOOKUP_POLL: Duration = Duration::from_millis(10);
/// How long the output of a lookup that ended is waited for.
const OUTPUT_GRACE: Duration = Duration::from_secs(1);

/// The ports [`super::free_port`] handed out to this test process.
static HANDED_OUT: Mutex<Vec<u16>> = Mutex::new(Vec::new());

/// Remembers that [`super::free_port`] handed out `port`.
pub fn handed_out(port: u16) {
    HANDED_OUT.lock().unwrap_or_else(PoisonError::into_inner).push(port);
}

/// Whether [`super::free_port`] handed out `port` to this test process.
fn was_handed_out(port: u16) -> bool {
    HANDED_OUT
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .contains(&port)
}

/// A Daemon a test started, as much of it as is needed to ask it for its Leases.
#[derive(Debug, Clone)]
struct StartedDaemon {
    state_dir: PathBuf,
    config_dir: PathBuf,
    /// The file the Daemon reads its clock from, when the test gave it one.
    clock: Option<PathBuf>,
}

thread_local! {
    /// The Daemons the current test started, a test running on its own thread.
    static DAEMONS: std::cell::RefCell<Vec<StartedDaemon>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Remembers that the current test runs `procly` with these directories.
pub fn remember_daemon(state_dir: &Path, config_dir: &Path, clock: Option<&Path>) {
    DAEMONS.with_borrow_mut(|daemons| {
        if daemons.iter().all(|daemon| daemon.state_dir != state_dir) {
            daemons.push(StartedDaemon {
                state_dir: state_dir.to_owned(),
                config_dir: config_dir.to_owned(),
                clock: clock.map(Path::to_owned),
            });
        }
    });
}

/// A program that lists listeners the way `lsof` does, with the arguments that go before the ones of the lookup.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub program: String,
    pub args: Vec<String>,
}

impl Candidate {
    /// `program` called with no argument of its own.
    fn plain(program: &str) -> Self {
        Self {
            program: program.to_owned(),
            args: Vec::new(),
        }
    }
}

/// How the lookup tools are called.
#[derive(Debug, Clone)]
pub struct Lookup {
    pub candidates: Vec<Candidate>,
    pub timeout: Duration,
}

impl Default for Lookup {
    /// `lsof` from the `PATH`, then from where macOS and Linux keep it, which a minimal `PATH` may lack.
    fn default() -> Self {
        Self {
            candidates: ["lsof", "/usr/sbin/lsof", "/usr/bin/lsof"]
                .map(Candidate::plain)
                .to_vec(),
            timeout: LOOKUP_TIMEOUT,
        }
    }
}

/// What holds `port` right now, as a few lines for a failure message.
pub fn diagnose(port: u16) -> String {
    diagnose_with(port, &Lookup::default())
}

/// Like [`diagnose`], looking the listeners up with `lookup`.
pub fn diagnose_with(port: u16, lookup: &Lookup) -> String {
    let mut lines = vec![format!(
        "port {port}, handed out by free_port(), at the time of the failure:"
    )];
    match listeners(port, lookup) {
        Err(reason) => lines.push(format!("- the holders could not be listed: {reason}")),
        Ok(listeners) if listeners.is_empty() => {
            lines.push(
                "- nothing listens on it any more (as lsof sees it, the processes of other users are not)".to_owned(),
            );
        }
        Ok(listeners) => {
            for listener in listeners {
                let command_line = command_line(listener.pid, lookup)
                    .unwrap_or_else(|reason| format!("{} (command line unavailable: {reason})", listener.command));
                lines.push(format!(
                    "- pid {} listens on {}: {command_line}",
                    listener.pid, listener.address
                ));
            }
        }
    }
    lines.extend(DAEMONS.with_borrow(|daemons| {
        daemons
            .iter()
            .filter_map(|daemon| lease_on(daemon, port))
            .collect::<Vec<_>>()
    }));
    lines.join("\n")
}

/// The diagnostic of each port a failing command reported a Port mismatch on and that [`super::free_port`] handed
/// out, preceded by a newline, or an empty string when there is none.
pub fn for_port_mismatch(report: &str) -> String {
    let mut ports = Vec::new();
    for line in report.lines().filter(|line| line.contains("Port mismatch")) {
        for (index, _) in line.match_indices("port ") {
            let digits: String = line[index + "port ".len()..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            if let Ok(port) = digits.parse::<u16>()
                && was_handed_out(port)
                && !ports.contains(&port)
            {
                ports.push(port);
            }
        }
    }
    ports.into_iter().fold(String::new(), |mut text, port| {
        text.push('\n');
        text.push_str(&diagnose(port));
        text
    })
}

/// A process listening on a port.
#[derive(Debug)]
struct Listener {
    pid: u32,
    command: String,
    address: String,
}

/// The processes listening on TCP `port`, read with `lsof`, or why it could not be read.
fn listeners(port: u16, lookup: &Lookup) -> Result<Vec<Listener>, String> {
    let address = format!("-iTCP:{port}");
    let arguments = ["+c", "0", "-nP", &address, "-sTCP:LISTEN", "-Fpcn"];
    let mut reasons = Vec::new();
    for candidate in &lookup.candidates {
        let program = &candidate.program;
        let mut command = Command::new(program);
        command.args(&candidate.args).args(arguments);
        match run_bounded(&mut command, lookup.timeout) {
            // lsof exits with 1 when no file matches.
            Ok((status, stdout)) if status.success() || status.code() == Some(1) => return Ok(parse(&stdout)),
            Ok((status, _)) => reasons.push(format!("{program} exited with {status}")),
            Err(Bounded::Missing) => reasons.push(format!("{program} was not found")),
            Err(failure) => return Err(format!("{program} {failure}")),
        }
    }
    Err(reasons.join(", "))
}

/// Reads the `p`, `c` and `n` fields of `lsof -Fpcn`.
fn parse(text: &str) -> Vec<Listener> {
    let mut listeners: Vec<Listener> = Vec::new();
    for line in text.lines() {
        let Some(field) = line.chars().next() else {
            continue;
        };
        let value = &line[field.len_utf8()..];
        match field {
            'p' => {
                if let Ok(pid) = value.parse() {
                    listeners.push(Listener {
                        pid,
                        command: String::new(),
                        address: String::new(),
                    });
                }
            }
            'c' => {
                if let Some(listener) = listeners.last_mut() {
                    value.clone_into(&mut listener.command);
                }
            }
            'n' => {
                if let Some(listener) = listeners.last_mut()
                    && listener.address.is_empty()
                {
                    value.clone_into(&mut listener.address);
                }
            }
            _ => {}
        }
    }
    listeners
}

/// The command line of the process `pid`, read with `ps`, which prints it the same way on macOS and on Linux.
fn command_line(pid: u32, lookup: &Lookup) -> Result<String, String> {
    let mut command = Command::new("ps");
    command.args(["-o", "args=", "-p", &pid.to_string()]);
    match run_bounded(&mut command, lookup.timeout) {
        Ok((status, stdout)) if status.success() && !stdout.trim().is_empty() => Ok(stdout.trim().to_owned()),
        Ok(_) => Err("the process has exited".to_owned()),
        Err(failure) => Err(format!("ps {failure}")),
    }
}

/// The Lease of `daemon` on `port` as a line, when its Daemon runs and one of its Services holds it.
fn lease_on(daemon: &StartedDaemon, port: u16) -> Option<String> {
    // Asking a Daemon that is not running would start one, and change what the failure is about.
    let pid = std::fs::read_to_string(daemon.state_dir.join("daemon.pid")).ok()?;
    if !super::is_alive(Pid::from_raw(pid.trim().parse().ok()?)) {
        return None;
    }
    let mut command = super::procly_binary(&daemon.state_dir, &daemon.config_dir, daemon.clock.as_deref());
    command
        .current_dir(&daemon.state_dir)
        .args(["status", "--all", "--json"]);
    let (status, stdout) = run_bounded(&mut command, LOOKUP_TIMEOUT).ok()?;
    if !status.success() {
        return None;
    }
    let status: serde_json::Value = serde_json::from_str(&stdout).ok()?;
    status["repositories"]
        .as_array()?
        .iter()
        .flat_map(|repository| repository["worktrees"].as_array().into_iter().flatten())
        .find_map(|worktree| {
            let service = worktree["services"]
                .as_array()?
                .iter()
                .find(|service| service["port"].as_u64() == Some(u64::from(port)))?;
            Some(format!(
                "- the Lease of the Service {} in the Worktree {} holds it",
                service["service"].as_str()?,
                worktree["path"].as_str()?
            ))
        })
}

/// Why a bounded run gave no output.
#[derive(Debug)]
enum Bounded {
    /// The program does not exist at this place.
    Missing,
    /// The program ran past the time it had and was killed.
    TimedOut(Duration),
    /// The program could not be started, or its end could not be awaited.
    Failed(std::io::Error),
}

impl std::fmt::Display for Bounded {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing => write!(formatter, "was not found"),
            Self::TimedOut(timeout) => write!(formatter, "did not answer within {timeout:?}"),
            Self::Failed(error) => write!(formatter, "failed: {error}"),
        }
    }
}

/// Runs `command` and returns its exit status and what it printed on stdout, killing it once `timeout` has passed.
fn run_bounded(command: &mut Command, timeout: Duration) -> Result<(ExitStatus, String), Bounded> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => Bounded::Missing,
            _ => Bounded::Failed(error),
        })?;
    let mut stdout = child.stdout.take().expect("stdout was piped");
    let (sender, receiver) = mpsc::channel();
    // A reader of its own, so that a lookup printing more than a pipe holds is not stuck on a full pipe.
    std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stdout.read_to_string(&mut text);
        let _ = sender.send(text);
    });
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok((status, receiver.recv_timeout(OUTPUT_GRACE).unwrap_or_default())),
            Ok(None) if start.elapsed() < timeout => std::thread::sleep(LOOKUP_POLL),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Bounded::TimedOut(timeout));
            }
            Err(error) => return Err(Bounded::Failed(error)),
        }
    }
}
