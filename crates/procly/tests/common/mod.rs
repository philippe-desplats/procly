//! Test harness shared by the integration tests: one isolated Daemon per test.

#![allow(
    dead_code,
    unreachable_pub,
    reason = "each integration test binary compiles this module and uses a different part of it"
)]

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use nix::errno::Errno;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use tempfile::TempDir;

/// How long a test waits for an asynchronous effect before failing.
pub const DEADLINE: Duration = Duration::from_secs(10);

/// A temporary state directory, so the Daemon started by the first command never touches the user's real one.
///
/// Dropping it stops the Daemon, which kills every Job it still runs.
#[derive(Debug)]
pub struct Procly {
    state_dir: TempDir,
    scratch: TempDir,
}

impl Procly {
    pub fn new() -> Self {
        // A short prefix under the system temp dir keeps the socket path below the 104-byte limit of macOS.
        let state_dir = tempfile::Builder::new()
            .prefix("pcl")
            .tempdir()
            .expect("create state dir");
        let scratch = tempfile::Builder::new()
            .prefix("pcl-work")
            .tempdir()
            .expect("create scratch dir");
        Self { state_dir, scratch }
    }

    pub fn state_dir(&self) -> &Path {
        self.state_dir.path()
    }

    /// A directory the test may use as a working directory or to exchange files with a Job.
    pub fn scratch(&self) -> &Path {
        self.scratch.path()
    }

    /// The `procly` binary, pointed at this test's state directory and run from the scratch directory.
    pub fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_procly"));
        command
            .env("PROCLY_STATE_DIR", self.state_dir())
            .current_dir(self.scratch());
        command
    }

    pub fn procly(&self, args: &[&str]) -> Output {
        self.command().args(args).output().expect("the procly binary runs")
    }

    /// Runs `procly run` with the given arguments and returns the Job identifier it printed.
    pub fn run(&self, args: &[&str]) -> String {
        let output = self.procly(&[&["run"], args].concat());
        assert_success(&output);
        let job = stdout(&output).trim().to_owned();
        assert!(
            !job.is_empty() && !job.contains(char::is_whitespace),
            "unexpected job id {job:?}"
        );
        job
    }

    fn daemon_pid(&self) -> Option<Pid> {
        let pid = std::fs::read_to_string(self.state_dir().join("daemon.pid")).ok()?;
        Some(Pid::from_raw(pid.trim().parse().ok()?))
    }
}

impl Drop for Procly {
    fn drop(&mut self) {
        let Some(pid) = self.daemon_pid() else { return };
        if kill(pid, Signal::SIGTERM).is_ok() {
            wait_until_gone(pid);
        }
    }
}

pub fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

pub fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

pub fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "exit status: {}\nstdout: {}\nstderr: {}",
        output.status,
        stdout(output),
        stderr(output)
    );
}

/// Polls `condition` until it holds, failing the test after [`DEADLINE`].
pub fn eventually(what: &str, mut condition: impl FnMut() -> bool) {
    let start = Instant::now();
    while !condition() {
        assert!(start.elapsed() < DEADLINE, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

pub fn is_alive(pid: Pid) -> bool {
    // A zombie still answers signal 0, it is gone once its parent or init reaps it.
    !matches!(kill(pid, None), Err(Errno::ESRCH))
}

pub fn wait_until_gone(pid: Pid) {
    eventually(&format!("process {pid} to exit"), || !is_alive(pid));
}

/// Reads a pid a Job wrote into a file, waiting for the file to appear.
pub fn read_pid(path: &Path) -> Pid {
    let mut pid = None;
    eventually(&format!("pid file {}", path.display()), || {
        pid = std::fs::read_to_string(path).ok().and_then(|s| s.trim().parse().ok());
        pid.is_some()
    });
    Pid::from_raw(pid.expect("the pid was read"))
}

/// The path of the bundled test server fixture.
pub fn test_server() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_procly-test-server"))
}

/// A port nothing listens on right now.
pub fn free_port() -> u16 {
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .expect("bind an ephemeral port")
        .local_addr()
        .expect("read the ephemeral port")
        .port()
}

/// Sends `GET <path>` to the test server on `port`, retrying until it accepts connections, and returns the body.
pub fn http_get(port: u16, path: &str) -> String {
    let mut body = None;
    eventually(&format!("the test server on port {port}"), || {
        let Ok(mut stream) = std::net::TcpStream::connect(("127.0.0.1", port)) else {
            return false;
        };
        let request = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n");
        let mut response = String::new();
        if stream.write_all(request.as_bytes()).is_err() || stream.read_to_string(&mut response).is_err() {
            return false;
        }
        body = response.split_once("\r\n\r\n").map(|(_, body)| body.to_owned());
        body.is_some()
    });
    body.expect("the response was read")
}
