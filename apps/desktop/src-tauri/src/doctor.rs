//! `procly doctor --json`, run through the `procly` the app embeds and never as a request to the Daemon: the checks of
//! the installation live in the CLI and must still report a Daemon that does not run (ADR 0009, `docs/rules/tauri.md`).
//!
//! The document comes back to the window as the CLI printed it. Nothing here runs on a timer: the window asks when a
//! screen that shows the checks opens, and on Refresh.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use procly_client::state_dir::{STATE_DIR_ENV, StateDir};
use serde_json::Value;
use tauri::State;

use crate::daemon::{CommandError, DaemonClient};

/// The exit code of `procly doctor` when at least one check failed. The document is printed all the same.
const EXIT_PROBLEMS: i32 = 7;

/// The directory the machine and the Daemon are checked from: it is in no git Repository, so the checks that belong
/// to a Worktree are skipped and say so.
const NO_WORKTREE: &str = "/";

impl DaemonClient {
    /// Runs `procly doctor --json` of the embedded `procly` against the Daemon of the state directory the app reads,
    /// from `cwd` or from no Worktree, and returns the document it prints. It never starts a Daemon.
    ///
    /// # Errors
    ///
    /// [`CommandError::Sidecar`] when the embedded `procly` cannot run, and [`CommandError::Doctor`] when `cwd` is not
    /// a directory, when doctor fails for another reason than finding problems, or when it prints no document.
    pub(crate) async fn doctor(&self, cwd: Option<PathBuf>) -> Result<Value, CommandError> {
        let state = self.state().clone();
        let procly = self.procly().to_path_buf();
        let cwd = cwd.unwrap_or_else(|| PathBuf::from(NO_WORKTREE));
        tauri::async_runtime::spawn_blocking(move || run(&state, &procly, &cwd))
            .await
            .map_err(CommandError::Task)?
    }
}

fn run(state: &StateDir, procly: &Path, cwd: &Path) -> Result<Value, CommandError> {
    // A Worktree that was removed has no directory to check from, say so rather than blame the embedded `procly`.
    if !cwd.is_absolute() || !cwd.is_dir() {
        return Err(CommandError::Doctor {
            message: format!(
                "{} is not a directory, doctor has nothing to check there",
                cwd.display()
            ),
        });
    }
    let output = Command::new(procly)
        .args(["doctor", "--json"])
        // The Daemon the app reads, whatever the environment the app was opened with resolves to.
        .env(STATE_DIR_ENV, state.path())
        .current_dir(cwd)
        .stdin(Stdio::null())
        .output()
        .map_err(|source| CommandError::Sidecar {
            path: procly.to_path_buf(),
            source,
        })?;
    // Doctor exits with 7 when a check failed, which is an answer and not a failure of doctor.
    let answered = output.status.success() || output.status.code() == Some(EXIT_PROBLEMS);
    if !answered {
        let written = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(CommandError::Doctor {
            message: if written.is_empty() {
                format!("procly doctor {}", output.status)
            } else {
                written
            },
        });
    }
    serde_json::from_slice(&output.stdout).map_err(|error| CommandError::Doctor {
        message: format!("procly doctor printed no document the app can read: {error}"),
    })
}

/// The checks of the installation, from `cwd` when the window shows a Worktree, from no Worktree otherwise.
///
/// # Errors
///
/// See [`DaemonClient::doctor`].
#[tauri::command]
pub(crate) async fn doctor(cwd: Option<PathBuf>, client: State<'_, DaemonClient>) -> Result<Value, CommandError> {
    client.doctor(cwd).await
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use procly_client::state_dir::StateDir;

    use crate::daemon::{CommandError, DaemonClient};
    use crate::stand_in;

    /// A temporary directory holding the state directory, a Worktree and the stand-in for the embedded `procly`.
    struct Scene {
        dir: tempfile::TempDir,
        state: StateDir,
    }

    impl Scene {
        fn new() -> Self {
            let dir = tempfile::Builder::new()
                .prefix("pdd")
                .tempdir()
                .expect("the temporary directory can be created");
            let state = StateDir::at(dir.path().join("state"));
            fs::create_dir(dir.path().join("web")).expect("the Worktree directory can be created");
            Self { dir, state }
        }

        fn worktree(&self) -> PathBuf {
            self.dir
                .path()
                .join("web")
                .canonicalize()
                .expect("the Worktree directory exists")
        }

        fn calls(&self) -> PathBuf {
            self.dir.path().join("calls")
        }

        /// A stand-in for the embedded `procly` that records its arguments, the state directory and the directory it
        /// ran in, then prints `stdout` and `stderr` and exits with `code`.
        fn procly(&self, code: i32, stdout: &str, stderr: &str) -> PathBuf {
            let path = self.dir.path().join("procly");
            let script = format!(
                "#!/bin/sh\necho \"$@ $PROCLY_STATE_DIR $(pwd -P)\" >> {calls}\n\
                 printf '%s' '{stdout}'\nprintf '%s' '{stderr}' >&2\nexit {code}\n",
                calls = self.calls().display(),
            );
            stand_in::write(&path, &script);
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
    }

    const DOCUMENT: &str = r#"{"schema_version":1,"result":"healthy","checks":[],"prompt":null,"agent":null}"#;

    #[test]
    fn doctor_runs_the_embedded_procly_in_json_against_the_state_directory_the_app_reads() {
        let scene = Scene::new();
        let procly = scene.procly(0, DOCUMENT, "");

        let document = tauri::async_runtime::block_on(scene.client(procly).doctor(Some(scene.worktree())))
            .expect("doctor answers");

        assert_eq!(document["result"], "healthy");
        let state = scene.state.path().display().to_string();
        let worktree = scene.worktree().display().to_string();
        assert_eq!(scene.recorded(), [format!("doctor --json {state} {worktree}")]);
    }

    #[test]
    fn without_a_worktree_doctor_runs_from_a_directory_that_is_in_no_repository() {
        let scene = Scene::new();
        let procly = scene.procly(0, DOCUMENT, "");

        tauri::async_runtime::block_on(scene.client(procly).doctor(None)).expect("doctor answers");

        let calls = scene.recorded();
        assert!(calls[0].ends_with(" /"), "{calls:?}");
    }

    #[test]
    fn a_doctor_that_found_problems_still_hands_over_its_document() {
        let scene = Scene::new();
        let problems = r#"{"schema_version":1,"result":"problems","checks":[],"prompt":"fix it","agent":null}"#;
        let procly = scene.procly(7, problems, "procly: 1 check failed");

        let document = tauri::async_runtime::block_on(scene.client(procly).doctor(None)).expect("exit 7 is an answer");

        assert_eq!(document["result"], "problems");
        assert_eq!(document["prompt"], "fix it");
    }

    #[test]
    fn a_doctor_that_fails_is_reported_as_it_wrote() {
        let scene = Scene::new();
        let procly = scene.procly(125, "", "procly: cannot read the state directory");

        let error = tauri::async_runtime::block_on(scene.client(procly).doctor(None))
            .expect_err("exit 125 is a failure of doctor");

        assert!(matches!(error, CommandError::Doctor { .. }), "{error:?}");
        let json = serde_json::to_value(&error).expect("the error serializes");
        assert_eq!(json["kind"], "doctor");
        assert_eq!(json["message"], "procly: cannot read the state directory");
    }

    #[test]
    fn a_doctor_that_prints_no_document_is_a_failure_and_not_an_empty_report() {
        let scene = Scene::new();
        let procly = scene.procly(0, "not json", "");

        let error = tauri::async_runtime::block_on(scene.client(procly).doctor(None))
            .expect_err("text that is no document cannot be handed over");

        let json = serde_json::to_value(&error).expect("the error serializes");
        assert_eq!(json["kind"], "doctor");
    }

    #[test]
    fn a_worktree_with_no_directory_is_named_and_doctor_is_not_run() {
        let scene = Scene::new();
        let procly = scene.procly(0, DOCUMENT, "");
        let removed = scene.dir.path().join("removed");

        let error = tauri::async_runtime::block_on(scene.client(procly).doctor(Some(removed.clone())))
            .expect_err("a directory that is gone cannot be checked");

        let json = serde_json::to_value(&error).expect("the error serializes");
        assert_eq!(json["kind"], "doctor");
        assert!(
            json["message"]
                .as_str()
                .is_some_and(|message| message.contains(&*removed.to_string_lossy()))
        );
        assert!(scene.recorded().is_empty(), "doctor was not run");
    }

    #[test]
    fn a_relative_directory_is_refused() {
        let scene = Scene::new();
        let procly = scene.procly(0, DOCUMENT, "");

        let error = tauri::async_runtime::block_on(scene.client(procly).doctor(Some(PathBuf::from("web"))))
            .expect_err("only an absolute directory names a Worktree");

        assert!(matches!(error, CommandError::Doctor { .. }), "{error:?}");
        assert!(scene.recorded().is_empty(), "doctor was not run");
    }

    #[test]
    fn a_procly_that_cannot_run_is_named_and_starts_nothing() {
        let scene = Scene::new();
        let missing = scene.dir.path().join("not-there").join("procly");

        let error = tauri::async_runtime::block_on(scene.client(missing.clone()).doctor(None))
            .expect_err("a procly that is not there cannot check anything");

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
