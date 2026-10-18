//! Test harness shared by the integration tests: one isolated Daemon per test.

#![allow(
    dead_code,
    unreachable_pub,
    reason = "each integration test binary compiles this module and uses a different part of it"
)]

use std::io::{Read as _, Write as _};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use nix::errno::Errno;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use tempfile::TempDir;
use tokio::net::TcpSocket;

/// How long a test waits for an asynchronous effect before failing.
pub const DEADLINE: Duration = Duration::from_secs(10);

/// A temporary state directory, so the Daemon started by the first command never touches the user's real one.
///
/// Dropping it stops the Daemon, which kills every Job it still runs.
#[derive(Debug)]
pub struct Procly {
    state_dir: TempDir,
    config_dir: TempDir,
    scratch: TempDir,
    /// The file the Daemon reads its clock from, set by [`Procly::with_clock`].
    clock: Option<PathBuf>,
}

impl Procly {
    pub fn new() -> Self {
        // A short prefix under the system temp dir keeps the socket path below the 104-byte limit of macOS.
        let state_dir = tempfile::Builder::new()
            .prefix("pcl")
            .tempdir()
            .expect("create state dir");
        let config_dir = tempfile::Builder::new()
            .prefix("pcl-config")
            .tempdir()
            .expect("create config dir");
        let scratch = tempfile::Builder::new()
            .prefix("pcl-work")
            .tempdir()
            .expect("create scratch dir");
        Self {
            state_dir,
            config_dir,
            scratch,
            clock: None,
        }
    }

    /// Like [`Procly::new`], with a Daemon whose clock stands still until [`Procly::advance_clock`] moves it, so
    /// that backoff and idle stop are tested without waiting.
    pub fn with_clock() -> Self {
        let mut procly = Self::new();
        let clock = procly.state_dir().join("test-clock");
        procly.clock = Some(clock);
        procly.advance_clock(Duration::ZERO);
        procly
    }

    /// Moves the clock of the Daemon forward by `by`. The file is replaced, so the Daemon never reads half of it.
    pub fn advance_clock(&self, by: Duration) {
        let clock = self.clock.as_ref().expect("the test was built with Procly::with_clock");
        let now: u64 = std::fs::read_to_string(clock).map_or(0, |text| text.trim().parse().unwrap_or(0));
        let next = now + u64::try_from(by.as_millis()).expect("a duration in milliseconds fits in u64");
        let pending = clock.with_extension("next");
        std::fs::write(&pending, next.to_string()).expect("write the next clock reading");
        std::fs::rename(&pending, clock).expect("replace the clock reading");
    }

    /// What the Daemon logged so far.
    pub fn daemon_log(&self) -> String {
        std::fs::read_to_string(self.state_dir().join("daemon.log")).unwrap_or_default()
    }

    pub fn state_dir(&self) -> &Path {
        self.state_dir.path()
    }

    /// The directory of the user settings, so that no test reads the developer's real ones.
    pub fn config_dir(&self) -> &Path {
        self.config_dir.path()
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
            .env("PROCLY_CONFIG_DIR", self.config_dir())
            .current_dir(self.scratch());
        if let Some(clock) = &self.clock {
            // Set on every command: the Daemon takes the environment of the one that starts it.
            command.env("PROCLY_TEST_CLOCK", clock);
        }
        command
    }

    pub fn procly(&self, args: &[&str]) -> Output {
        self.command().args(args).output().expect("the procly binary runs")
    }

    /// Runs `procly` with the given arguments from `dir`.
    pub fn procly_in(&self, dir: &Path, args: &[&str]) -> Output {
        self.command()
            .current_dir(dir)
            .args(args)
            .output()
            .expect("the procly binary runs")
    }

    /// Runs `procly run` with the given arguments and returns the Job identifier it printed.
    pub fn run(&self, args: &[&str]) -> String {
        self.start(args).0
    }

    /// Runs `procly run` with the given arguments and returns the Job identifier and the outcome it printed.
    pub fn start(&self, args: &[&str]) -> (String, String) {
        let output = self.procly(&[&["run"], args].concat());
        assert_success(&output);
        started(&output)
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

/// Reads what `procly run` printed: the Job identifier alone on stdout, and the outcome alone on stderr.
pub fn started(output: &Output) -> (String, String) {
    let (job, outcome) = (stdout(output), stderr(output));
    assert!(
        job.ends_with('\n') && job.lines().count() == 1 && !job.trim().is_empty() && !job.trim().contains(' '),
        "unexpected run stdout {job:?}"
    );
    assert!(
        matches!(outcome.as_str(), "started\n" | "reused\n"),
        "unexpected run stderr {outcome:?}"
    );
    (job.trim().to_owned(), outcome.trim().to_owned())
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

/// `git` isolated from the developer's configuration and from any `GIT_*` variable of a calling hook.
pub fn git(dir: &Path, args: &[&str]) {
    let mut command = Command::new("git");
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("GIT_") {
            command.env_remove(name);
        }
    }
    let output = command
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["-c", "user.name=Procly Test", "-c", "user.email=test@example.com"])
        .args(["-c", "commit.gpgsign=false", "-c", "init.defaultBranch=main"])
        .args(args)
        .output()
        .expect("git runs");
    assert_success(&output);
}

/// Creates a Repository with one commit in `parent/name`, and returns its Main worktree, canonicalized.
pub fn git_repository(parent: &Path, name: &str) -> PathBuf {
    git(parent, &["init", "--quiet", name]);
    let main = parent.join(name).canonicalize().expect("canonicalize the repository");
    git(&main, &["commit", "--quiet", "--allow-empty", "--message", "initial"]);
    main
}

/// Adds a Worktree next to the Main worktree, on a new branch, and returns it canonicalized.
pub fn git_worktree_add(main: &Path, name: &str) -> PathBuf {
    let path = main.parent().expect("the Main worktree has a parent").join(name);
    git(main, &["worktree", "add", "--quiet", &path.to_string_lossy()]);
    path.canonicalize().expect("canonicalize the worktree")
}

/// The path of the bundled test server fixture.
pub fn test_server() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_procly-test-server"))
}

/// A port nothing listens on right now, and that the Daemon will accept as a Lease when a test declares it.
///
/// The Daemon checks a port by binding the wildcard addresses without `SO_REUSEADDR`, which the standard library
/// sets. A port in `TIME_WAIT` passes the standard bind and fails the Daemon's, and the ephemeral ports this hands out
/// are the ones the suite's own connections use as source ports and leave in `TIME_WAIT` when they close first. Under
/// load that sent the Lease of a test to the Range, away from the port it declared.
pub fn free_port() -> u16 {
    loop {
        let port = std::net::TcpListener::bind(("127.0.0.1", 0))
            .expect("bind an ephemeral port")
            .local_addr()
            .expect("read the ephemeral port")
            .port();
        if accepted_by_the_daemon(port) {
            return port;
        }
    }
}

/// Whether the bind the Daemon checks a Lease with succeeds on `port`, IPv4 and IPv6.
fn accepted_by_the_daemon(port: u16) -> bool {
    let v4 = TcpSocket::new_v4().and_then(|socket| socket.bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, port))));
    if v4.is_err() {
        return false;
    }
    match TcpSocket::new_v6().and_then(|socket| socket.bind(SocketAddr::from((Ipv6Addr::UNSPECIFIED, port)))) {
        Ok(()) => true,
        // A host without IPv6 has nothing listening on it.
        Err(error) => matches!(
            error.raw_os_error(),
            Some(nix::libc::EAFNOSUPPORT | nix::libc::EADDRNOTAVAIL)
        ),
    }
}

/// Sends `GET <path>` to the test server on `port`, retrying until it accepts connections, and returns the body.
pub fn http_get(port: u16, path: &str) -> String {
    let response = http_response(port, path);
    let (_, body) = response.split_once("\r\n\r\n").expect("the response has a body");
    body.to_owned()
}

/// Sends `GET <path>` to the test server on `port`, retrying until it accepts connections, and returns the whole
/// response.
pub fn http_response(port: u16, path: &str) -> String {
    let mut response = None;
    eventually(&format!("the test server on port {port}"), || {
        let Ok(mut stream) = std::net::TcpStream::connect(("127.0.0.1", port)) else {
            return false;
        };
        let request = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n");
        let mut text = String::new();
        if stream.write_all(request.as_bytes()).is_err() || stream.read_to_string(&mut text).is_err() {
            return false;
        }
        response = Some(text);
        true
    });
    response.expect("the response was read")
}

impl Procly {
    /// Gives the Daemon a narrow Range of its own in the user settings, so that tests running in parallel, each with
    /// its own Daemon, never draw the same ports. Returns its bounds.
    ///
    /// The Ranges sit below the ephemeral ports of Linux and macOS, so that [`free_port`] never returns one of them,
    /// and each test of a binary gets the next one, offset by the pid against other test runs on the same machine.
    pub fn narrow_range(&self) -> (u16, u16) {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let slot = (std::process::id() * 7 + NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)) % 600;
        let start = 20_000 + u16::try_from(slot * 20).expect("600 slots of 20 ports fit below 32768");
        let end = start + 19;
        std::fs::write(
            self.config_dir().join("config.toml"),
            format!("[settings]\nport_range = \"{start}-{end}\"\n"),
        )
        .expect("write the user settings");
        (start, end)
    }

    /// Stops the Daemon and waits until it has exited, the next command starts a new one.
    pub fn stop_daemon(&self) {
        let pid = self.daemon_pid().expect("a Daemon runs");
        kill(pid, Signal::SIGTERM).expect("signal the Daemon");
        wait_until_gone(pid);
    }
}

impl Procly {
    /// Kills the Daemon with `SIGKILL`, as a crash would, and waits until it has exited. It cleans nothing up, so its
    /// socket, its pid file and the processes it supervised are left behind for the next Daemon to find.
    pub fn kill_daemon(&self) {
        let pid = self.daemon_pid().expect("a Daemon runs");
        kill(pid, Signal::SIGKILL).expect("kill the Daemon");
        wait_until_gone(pid);
    }
}

/// Like [`git`], and returns what the command printed on stdout.
pub fn git_output(dir: &Path, args: &[&str]) -> String {
    let mut command = Command::new("git");
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("GIT_") {
            command.env_remove(name);
        }
    }
    let output = command
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(args)
        .output()
        .expect("git runs");
    assert_success(&output);
    stdout(&output)
}

/// Whether `docker compose` v2 and a running Docker engine are available, printing why a Docker test is skipped
/// otherwise. A Docker test starts with `if !require_docker() { return; }`.
pub fn require_docker() -> bool {
    let works = |args: &[&str]| {
        Command::new("docker")
            .args(args)
            .stdin(std::process::Stdio::null())
            .output()
            .is_ok_and(|output| output.status.success())
    };
    let available = works(&["compose", "version"]) && works(&["info"]);
    if !available {
        eprintln!("skipped: docker compose v2 and a running Docker engine are required");
    }
    available
}
