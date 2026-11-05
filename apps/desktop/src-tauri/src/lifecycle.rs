//! The two things the developer can ask of the Daemon's lifecycle, each through the `procly` the app embeds and never
//! on its own: start it, and restart one of another version (`docs/rules/tauri.md`). Everything else the app does is
//! to read the Daemon with a bare `connect`.

use std::path::Path;
use std::process::{Command, Stdio};

use procly_client::connect_or_start;
use procly_client::state_dir::{STATE_DIR_ENV, StateDir};
use tauri::State;

use crate::daemon::{CommandError, DaemonClient};
use crate::poller::Poller;

impl DaemonClient {
    /// Starts the Daemon with `procly daemon` of the embedded `procly`, and returns once it accepts connections. It
    /// does nothing when one already runs.
    ///
    /// # Errors
    ///
    /// When the embedded `procly` cannot start a Daemon that then accepts connections.
    pub(crate) async fn start(&self) -> Result<(), CommandError> {
        let state = self.state().clone();
        let procly = self.procly().to_path_buf();
        tauri::async_runtime::spawn_blocking(move || connect_or_start(&state, &procly).map(drop))
            .await
            .map_err(CommandError::Task)?
            .map_err(CommandError::from)
    }

    /// Stops the Daemon with `procly daemon stop --json`, which reaches a Daemon of any protocol version, then starts
    /// the one of the embedded `procly`. A stop that fails ends it there, no Daemon is started over the one still
    /// running.
    ///
    /// # Errors
    ///
    /// [`CommandError::Stop`] with what `daemon stop` wrote when it exits with an error, and the errors of
    /// [`DaemonClient::start`].
    pub(crate) async fn restart(&self) -> Result<(), CommandError> {
        let state = self.state().clone();
        let procly = self.procly().to_path_buf();
        tauri::async_runtime::spawn_blocking(move || {
            stop(&state, &procly)?;
            connect_or_start(&state, &procly).map(drop).map_err(CommandError::from)
        })
        .await
        .map_err(CommandError::Task)?
    }
}

/// Runs `procly daemon stop --json` against the Daemon of `state`, which waits at most 10 s for it to go.
fn stop(state: &StateDir, procly: &Path) -> Result<(), CommandError> {
    let output = Command::new(procly)
        .args(["daemon", "stop", "--json"])
        // The Daemon the app reads, whatever the environment the app was opened with resolves to.
        .env(STATE_DIR_ENV, state.path())
        .stdin(Stdio::null())
        .output()
        .map_err(|source| CommandError::Sidecar {
            path: procly.to_path_buf(),
            source,
        })?;
    if output.status.success() {
        return Ok(());
    }
    let written = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    Err(CommandError::Stop {
        message: if written.is_empty() {
            format!("procly daemon stop {}", output.status)
        } else {
            written
        },
    })
}

/// Starts the Daemon because the developer asked, then asks the poller for a tick so that the window recovers now.
///
/// # Errors
///
/// See [`DaemonClient::start`].
#[tauri::command]
pub(crate) async fn start_daemon(
    client: State<'_, DaemonClient>,
    poller: State<'_, Poller>,
) -> Result<(), CommandError> {
    client.start().await?;
    poller.refresh();
    Ok(())
}

/// Restarts a Daemon of another version because the developer confirmed it, then asks the poller for a tick.
///
/// # Errors
///
/// See [`DaemonClient::restart`].
#[tauri::command]
pub(crate) async fn restart_daemon(
    client: State<'_, DaemonClient>,
    poller: State<'_, Poller>,
) -> Result<(), CommandError> {
    client.restart().await?;
    poller.refresh();
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;
    use std::os::unix::net::UnixListener;
    use std::path::{Path, PathBuf};
    use std::thread;
    use std::time::{Duration, Instant};

    use procly_client::state_dir::StateDir;

    use crate::daemon::{CommandError, DaemonClient};

    /// A temporary directory short enough for the socket path limit of macOS, holding the state directory and the
    /// stand-in for the embedded `procly`.
    struct Scene {
        dir: tempfile::TempDir,
        state: StateDir,
    }

    impl Scene {
        fn new() -> Self {
            let dir = tempfile::Builder::new()
                .prefix("pdl")
                .tempdir()
                .expect("the temporary directory can be created");
            let state = StateDir::at(dir.path().join("state"));
            Self { dir, state }
        }

        /// Where the stand-in writes one line per call: its arguments, then the state directory it was given.
        fn calls(&self) -> PathBuf {
            self.dir.path().join("calls")
        }

        /// A stand-in for the embedded `procly` that records each call, and exits with `stop_code` when asked to
        /// `daemon stop`, writing `stop_message` on its standard error.
        fn procly(&self, stop_code: i32, stop_message: &str) -> PathBuf {
            let path = self.dir.path().join("procly");
            let script = format!(
                "#!/bin/sh\necho \"$@ $PROCLY_STATE_DIR\" >> {calls}\n\
                 if [ \"$1 $2\" = \"daemon stop\" ]; then echo '{stop_message}' >&2; exit {stop_code}; fi\n",
                calls = self.calls().display(),
            );
            fs::write(&path, script).expect("the stand-in can be written");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("the stand-in can be made executable");
            path
        }

        fn client(&self, procly: PathBuf) -> DaemonClient {
            DaemonClient::new(self.state.clone(), procly)
        }

        fn recorded(&self) -> Vec<String> {
            fs::read_to_string(self.calls())
                .unwrap_or_default()
                .lines()
                .map(str::to_owned)
                .collect()
        }

        /// Plays the Daemon a started `procly daemon` would become: binds the socket once the stand-in was called
        /// to start it, which is when `daemon` is the first word of a recorded call.
        fn daemon_after_start(&self) -> thread::JoinHandle<UnixListener> {
            let calls = self.calls();
            let socket = self.state.socket();
            thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(30);
                while !fs::read_to_string(&calls).unwrap_or_default().lines().any(is_start) {
                    assert!(
                        Instant::now() < deadline,
                        "the stand-in was never asked to start the Daemon"
                    );
                    thread::sleep(Duration::from_millis(10));
                }
                UnixListener::bind(socket).expect("the stand-in Daemon binds the socket")
            })
        }
    }

    /// Whether a recorded call is `daemon` alone, which starts a Daemon, rather than `daemon stop`.
    fn is_start(line: &str) -> bool {
        let mut words = line.split_whitespace();
        words.next() == Some("daemon") && words.next() != Some("stop")
    }

    #[test]
    fn start_the_daemon_runs_the_embedded_procly_with_daemon_and_waits_for_its_socket() {
        let scene = Scene::new();
        let procly = scene.procly(0, "");
        let daemon = scene.daemon_after_start();

        tauri::async_runtime::block_on(scene.client(procly).start()).expect("the Daemon starts");

        let _listener = daemon.join().expect("the stand-in Daemon ends");
        let calls = scene.recorded();
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert!(calls[0].starts_with("daemon "), "{calls:?}");
    }

    #[test]
    fn start_the_daemon_names_the_embedded_procly_when_it_cannot_run() {
        let scene = Scene::new();
        let missing = scene.dir.path().join("not-there").join("procly");

        let error = tauri::async_runtime::block_on(scene.client(missing.clone()).start())
            .expect_err("a procly that is not there cannot start a Daemon");

        let json = serde_json::to_value(&error).expect("the error serializes");
        assert_eq!(json["kind"], "start_daemon");
        assert!(
            json["message"]
                .as_str()
                .is_some_and(|message| message.contains(&*missing.to_string_lossy()))
        );
    }

    #[test]
    fn restart_the_daemon_stops_it_through_the_embedded_procly_then_starts_it_with_the_same_one() {
        let scene = Scene::new();
        let procly = scene.procly(0, "");
        let daemon = scene.daemon_after_start();

        tauri::async_runtime::block_on(scene.client(procly).restart()).expect("the Daemon restarts");

        let _listener = daemon.join().expect("the stand-in Daemon ends");
        let state = scene.state.path().display().to_string();
        let calls = scene.recorded();
        assert_eq!(calls.len(), 2, "{calls:?}");
        // The stop is asked of the Daemon of the state directory the app reads, in JSON, and only then the start.
        assert_eq!(calls[0], format!("daemon stop --json {state}"));
        assert!(calls[1].starts_with("daemon "), "{calls:?}");
    }

    #[test]
    fn a_stop_that_fails_is_reported_as_it_was_written_and_no_daemon_is_started() {
        let scene = Scene::new();
        let procly = scene.procly(125, "procly: the Daemon (pid 4242) did not stop within 10 s");

        let error = tauri::async_runtime::block_on(scene.client(procly).restart())
            .expect_err("a Daemon that does not stop is not restarted");

        assert!(matches!(error, CommandError::Stop { .. }), "{error:?}");
        let json = serde_json::to_value(&error).expect("the error serializes");
        assert_eq!(json["kind"], "daemon_stop");
        assert_eq!(
            json["message"],
            "procly: the Daemon (pid 4242) did not stop within 10 s"
        );
        // Only the stop ran: starting a Daemon would call `daemon` again and leave its log in the state directory.
        assert_eq!(scene.recorded().len(), 1, "{:?}", scene.recorded());
        assert!(!Path::new(&scene.state.log()).exists(), "no Daemon was started");
    }

    #[test]
    fn a_stop_that_cannot_run_names_the_embedded_procly_and_starts_nothing() {
        let scene = Scene::new();
        let missing = scene.dir.path().join("not-there").join("procly");

        let error = tauri::async_runtime::block_on(scene.client(missing.clone()).restart())
            .expect_err("a procly that is not there cannot stop a Daemon");

        let json = serde_json::to_value(&error).expect("the error serializes");
        assert_eq!(json["kind"], "sidecar");
        assert!(
            json["message"]
                .as_str()
                .is_some_and(|message| message.contains(&*missing.to_string_lossy()))
        );
        assert!(!scene.state.log().exists(), "no Daemon was started");
    }
}
