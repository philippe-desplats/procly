//! The Session unit: a launchd agent on macOS or a systemd user unit on Linux that starts the Daemon with the user's
//! session, installed, inspected and removed by the CLI itself.
//!
//! It runs in the CLI and never asks the Daemon, since it is what starts the Daemon. It restarts the Daemon only
//! when it fails: `procly daemon` exits with 0 when another Daemon already holds the lock, and restarting it then
//! would loop. Services are in process groups of their own and outlive a crash of the Daemon, so that the next one
//! re-attaches them, which the unit must not prevent by stopping them along with it.

use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config;
use crate::state_dir::{StateDir, StateDirError};

/// The launchd label of the Session unit, after the GitHub home of the project.
const LAUNCHD_LABEL: &str = "io.github.philippe-desplats.procly";
/// The name of the systemd user unit.
const SYSTEMD_UNIT: &str = "procly.service";

#[derive(Debug, thiserror::Error)]
pub(crate) enum UnitError {
    #[error("session units exist on macOS (launchd) and Linux (systemd) only")]
    Unsupported,
    #[error("cannot locate the home directory: set HOME")]
    NoHome,
    #[error(transparent)]
    StateDir(#[from] StateDirError),
    #[error("cannot locate the procly executable to run the daemon from")]
    LocateExe(#[source] io::Error),
    #[error("cannot write the session unit {path}")]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cannot remove the session unit {path}")]
    Remove {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cannot run {command}")]
    Run {
        command: String,
        #[source]
        source: io::Error,
    },
    #[error("{command} failed: {stderr}")]
    Failed { command: String, stderr: String },
}

/// The session manager that starts the Daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Manager {
    Launchd,
    Systemd,
}

impl Manager {
    /// The manager of this platform. Chosen at compile time but written as a value, so that both paths are built and
    /// checked on every host.
    fn current() -> Result<Self, UnitError> {
        if cfg!(target_os = "macos") {
            Ok(Self::Launchd)
        } else if cfg!(target_os = "linux") {
            Ok(Self::Systemd)
        } else {
            Err(UnitError::Unsupported)
        }
    }
}

/// What the Daemon of the Session unit runs, and where its unit file goes.
#[derive(Debug)]
pub(crate) struct Unit {
    manager: Manager,
    /// Where the unit file is written.
    pub(crate) path: PathBuf,
    exe: PathBuf,
    /// Set so that the Daemon of the unit uses the state and the settings this command resolved, whatever the
    /// environment the session manager gives it.
    env: Vec<(String, String)>,
    log: PathBuf,
    /// The user id launchd addresses the session by.
    uid: u32,
}

/// What `procly unit inspect` found.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Inspection {
    pub(crate) installed: bool,
    /// Whether the session manager starts the Daemon with the session.
    pub(crate) enabled: bool,
    /// Whether the Daemon of the unit runs now.
    pub(crate) running: bool,
}

impl Unit {
    /// The Session unit of this user, for the Daemon of the state directory this command resolves.
    pub(crate) fn resolve(state: &StateDir) -> Result<Self, UnitError> {
        let manager = Manager::current()?;
        let home = non_empty_env("HOME").ok_or(UnitError::NoHome)?;
        let path = match manager {
            Manager::Launchd => home.join("Library/LaunchAgents").join(format!("{LAUNCHD_LABEL}.plist")),
            Manager::Systemd => non_empty_env("XDG_CONFIG_HOME")
                .unwrap_or_else(|| home.join(".config"))
                .join("systemd/user")
                .join(SYSTEMD_UNIT),
        };
        let mut env = vec![(
            crate::state_dir::STATE_DIR_ENV.to_owned(),
            state.path().display().to_string(),
        )];
        if let Some(dir) = config::user_settings().as_deref().and_then(Path::parent) {
            env.push((config::CONFIG_DIR_ENV.to_owned(), dir.display().to_string()));
        }
        // The Daemon resolves Repositories with git, which may live where only the shell of the user looks.
        if let Some(path) = std::env::var_os("PATH").and_then(|path| path.into_string().ok()) {
            env.push(("PATH".to_owned(), path));
        }
        Ok(Self {
            manager,
            path,
            exe: std::env::current_exe().map_err(UnitError::LocateExe)?,
            env,
            log: state.log(),
            uid: nix::unistd::getuid().as_raw(),
        })
    }

    /// Writes the unit file, replacing any previous one, and has the session manager start the Daemon now and with
    /// every session.
    pub(crate) fn install(&self) -> Result<(), UnitError> {
        let content = match self.manager {
            Manager::Launchd => launchd_plist(&self.exe, &self.env, &self.log),
            Manager::Systemd => systemd_unit(&self.exe, &self.env, &self.log),
        };
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(|source| UnitError::Write {
                path: self.path.clone(),
                source,
            })?;
        }
        std::fs::write(&self.path, content).map_err(|source| UnitError::Write {
            path: self.path.clone(),
            source,
        })?;
        match self.manager {
            Manager::Launchd => {
                // A unit installed before is unloaded first, so that the new file is the one launchd reads.
                let _ = output(&launchctl(&["bootout", &self.service_target()]));
                run(&launchctl(&[
                    "bootstrap",
                    &self.domain(),
                    &self.path.display().to_string(),
                ]))
            }
            Manager::Systemd => {
                run(&systemctl(&["daemon-reload"]))?;
                run(&systemctl(&["enable", "--now", SYSTEMD_UNIT]))
            }
        }
    }

    /// Whether the unit is installed, enabled and running.
    pub(crate) fn inspect(&self) -> Result<Inspection, UnitError> {
        let installed = self.path.exists();
        let (enabled, running) = match self.manager {
            Manager::Launchd => {
                let print = output(&launchctl(&["print", &self.service_target()]))?;
                let loaded = print.status.success();
                let running = loaded && String::from_utf8_lossy(&print.stdout).contains("state = running");
                (loaded, running)
            }
            Manager::Systemd => {
                let answer = |verb| -> Result<String, UnitError> {
                    let output = output(&systemctl(&[verb, SYSTEMD_UNIT]))?;
                    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
                };
                (answer("is-enabled")? == "enabled", answer("is-active")? == "active")
            }
        };
        Ok(Inspection {
            installed,
            enabled,
            running,
        })
    }

    /// Stops the Daemon of the unit, with its Services, and removes the unit file. Returns whether there was one.
    pub(crate) fn remove(&self) -> Result<bool, UnitError> {
        if !self.path.exists() {
            return Ok(false);
        }
        match self.manager {
            // Fails when the unit is not loaded, which leaves only the file to remove.
            Manager::Launchd => {
                let _ = output(&launchctl(&["bootout", &self.service_target()]));
            }
            Manager::Systemd => run(&systemctl(&["disable", "--now", SYSTEMD_UNIT]))?,
        }
        std::fs::remove_file(&self.path).map_err(|source| UnitError::Remove {
            path: self.path.clone(),
            source,
        })?;
        if self.manager == Manager::Systemd {
            run(&systemctl(&["daemon-reload"]))?;
        }
        Ok(true)
    }

    fn domain(&self) -> String {
        format!("gui/{}", self.uid)
    }

    fn service_target(&self) -> String {
        format!("{}/{LAUNCHD_LABEL}", self.domain())
    }
}

fn launchctl(args: &[&str]) -> Vec<String> {
    std::iter::once("launchctl")
        .chain(args.iter().copied())
        .map(str::to_owned)
        .collect()
}

fn systemctl(args: &[&str]) -> Vec<String> {
    ["systemctl", "--user"]
        .into_iter()
        .chain(args.iter().copied())
        .map(str::to_owned)
        .collect()
}

/// Runs `argv`, found through `PATH`, and returns its output whatever its exit status.
fn output(argv: &[String]) -> Result<std::process::Output, UnitError> {
    Command::new(&argv[0])
        .args(&argv[1..])
        .output()
        .map_err(|source| UnitError::Run {
            command: argv.join(" "),
            source,
        })
}

/// Runs `argv` and fails with its error output unless it succeeds.
fn run(argv: &[String]) -> Result<(), UnitError> {
    let output = output(argv)?;
    if output.status.success() {
        return Ok(());
    }
    Err(UnitError::Failed {
        command: argv.join(" "),
        stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
    })
}

/// The launchd agent running `exe daemon` at login, restarted only when it fails, without launchd killing the
/// process groups the Daemon leaves behind.
fn launchd_plist(exe: &Path, env: &[(String, String)], log: &Path) -> String {
    let mut variables = String::new();
    for (name, value) in env {
        let _ = write!(
            variables,
            "\n\t\t<key>{}</key>\n\t\t<string>{}</string>",
            xml_escape(name),
            xml_escape(value)
        );
    }
    let exe = xml_escape(&exe.display().to_string());
    let log = xml_escape(&log.display().to_string());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{LAUNCHD_LABEL}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{exe}</string>
		<string>daemon</string>
	</array>
	<key>EnvironmentVariables</key>
	<dict>{variables}
	</dict>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<dict>
		<key>SuccessfulExit</key>
		<false/>
	</dict>
	<key>AbandonProcessGroup</key>
	<true/>
	<key>StandardOutPath</key>
	<string>{log}</string>
	<key>StandardErrorPath</key>
	<string>{log}</string>
</dict>
</plist>
"#
    )
}

/// The systemd user unit running `exe daemon` with the session, restarted only when it fails. `KillMode=process`
/// leaves the Services running when the Daemon crashes, a clean stop has the Daemon stop them itself.
fn systemd_unit(exe: &Path, env: &[(String, String)], log: &Path) -> String {
    let mut environment = String::new();
    for (name, value) in env {
        let _ = writeln!(environment, "Environment={}", systemd_quote(&format!("{name}={value}")));
    }
    let exe = systemd_quote(&exe.display().to_string()).replace('$', "$$");
    let log = log.display().to_string().replace('%', "%%");
    format!(
        "[Unit]\n\
         Description=Procly Daemon\n\
         \n\
         [Service]\n\
         ExecStart={exe} daemon\n\
         {environment}\
         Restart=on-failure\n\
         KillMode=process\n\
         StandardOutput=append:{log}\n\
         StandardError=append:{log}\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n"
    )
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// One double-quoted word of a systemd unit, its specifiers escaped.
fn systemd_quote(text: &str) -> String {
    let escaped = text.replace('\\', "\\\\").replace('"', "\\\"").replace('%', "%%");
    format!("\"{escaped}\"")
}

fn non_empty_env(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> Vec<(String, String)> {
        vec![
            (
                "PROCLY_STATE_DIR".to_owned(),
                "/home/dev/.local/state/procly".to_owned(),
            ),
            ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
        ]
    }

    #[test]
    fn the_launchd_agent_runs_the_daemon_at_login_and_restarts_it_only_on_failure() {
        let plist = launchd_plist(
            Path::new("/opt/procly/bin/procly"),
            &env(),
            Path::new("/home/dev/.local/state/procly/daemon.log"),
        );
        assert_eq!(
            plist,
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>io.github.philippe-desplats.procly</string>
	<key>ProgramArguments</key>
	<array>
		<string>/opt/procly/bin/procly</string>
		<string>daemon</string>
	</array>
	<key>EnvironmentVariables</key>
	<dict>
		<key>PROCLY_STATE_DIR</key>
		<string>/home/dev/.local/state/procly</string>
		<key>PATH</key>
		<string>/usr/bin:/bin</string>
	</dict>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<dict>
		<key>SuccessfulExit</key>
		<false/>
	</dict>
	<key>AbandonProcessGroup</key>
	<true/>
	<key>StandardOutPath</key>
	<string>/home/dev/.local/state/procly/daemon.log</string>
	<key>StandardErrorPath</key>
	<string>/home/dev/.local/state/procly/daemon.log</string>
</dict>
</plist>
"#
        );
    }

    #[test]
    fn the_systemd_unit_runs_the_daemon_with_the_session_and_leaves_its_services_on_a_crash() {
        let unit = systemd_unit(
            Path::new("/opt/procly/bin/procly"),
            &env(),
            Path::new("/home/dev/.local/state/procly/daemon.log"),
        );
        assert_eq!(
            unit,
            "[Unit]\n\
             Description=Procly Daemon\n\
             \n\
             [Service]\n\
             ExecStart=\"/opt/procly/bin/procly\" daemon\n\
             Environment=\"PROCLY_STATE_DIR=/home/dev/.local/state/procly\"\n\
             Environment=\"PATH=/usr/bin:/bin\"\n\
             Restart=on-failure\n\
             KillMode=process\n\
             StandardOutput=append:/home/dev/.local/state/procly/daemon.log\n\
             StandardError=append:/home/dev/.local/state/procly/daemon.log\n\
             \n\
             [Install]\n\
             WantedBy=default.target\n"
        );
    }

    #[test]
    fn values_are_escaped_for_the_file_format_they_land_in() {
        assert_eq!(xml_escape("a & <b>"), "a &amp; &lt;b&gt;");
        assert_eq!(systemd_quote(r#"a "b" \ 100%"#), r#""a \"b\" \\ 100%%""#);
        let unit = systemd_unit(
            Path::new("/opt/my $app/procly"),
            &[],
            Path::new("/logs/100%/daemon.log"),
        );
        assert!(unit.contains("ExecStart=\"/opt/my $$app/procly\" daemon\n"), "{unit}");
        assert!(
            unit.contains("StandardOutput=append:/logs/100%%/daemon.log\n"),
            "{unit}"
        );
    }
}
