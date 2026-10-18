//! The output of `procly status`: compact text by default, versioned JSON on request.

use std::fmt::Write as _;
use std::path::Path;
use std::time::Duration;

use serde::Serialize;

use crate::duration;
use crate::protocol::{
    ForeignProcess, JobEnd, JobState, JobStatus, ServiceEnd, ServiceState, ServiceStatus, StatusReport,
};

/// The version of the `--json` schema, bumped on any incompatible change, independently of the socket protocol.
pub(crate) const JSON_SCHEMA_VERSION: u32 = 1;

pub(crate) fn json(report: &StatusReport) -> String {
    #[derive(Serialize)]
    struct Versioned<'a> {
        schema_version: u32,
        #[serde(flatten)]
        report: &'a StatusReport,
    }
    let versioned = Versioned {
        schema_version: JSON_SCHEMA_VERSION,
        report,
    };
    let mut json = serde_json::to_string(&versioned)
        .expect("a status report decoded from JSON holds only UTF-8 paths, so it serializes back");
    json.push('\n');
    json
}

/// One line per Repository, Worktree, Service and Job, indented by nesting.
pub(crate) fn text(report: &StatusReport) -> String {
    let mut text = String::new();
    for repository in &report.repositories {
        let _ = writeln!(text, "repository {}", repository.git_common_dir.display());
        for status in &repository.worktrees {
            let worktree = &status.worktree;
            let main = if worktree.main { " (main)" } else { "" };
            let _ = writeln!(text, "  worktree {}{main} {}", worktree.slug, worktree.path.display());
            for service in &status.services {
                service_line(&mut text, service);
            }
            for foreign in &status.foreign {
                foreign_line(&mut text, foreign);
            }
            if status.jobs.is_empty() {
                text.push_str("    no jobs\n");
            }
            for job in &status.jobs {
                job_line(&mut text, "    ", job, &worktree.path);
            }
        }
    }
    if !report.jobs_outside_repositories.is_empty() {
        text.push_str("outside any repository\n");
        for job in &report.jobs_outside_repositories {
            job_line(&mut text, "  ", job, Path::new(""));
        }
    }
    if text.is_empty() {
        text.push_str("no jobs\n");
    }
    text
}

fn service_line(text: &mut String, service: &ServiceStatus) {
    let port = service.port.map(|port| format!(" port {port}")).unwrap_or_default();
    let state = match service.state {
        ServiceState::Starting => format!("starting{port}"),
        ServiceState::Healthy => format!("healthy{port}"),
        ServiceState::Restarting => format!("restarting{port}"),
        ServiceState::Ended { end } => format!("ended: {}", ended(end)),
    };
    let reattached = if service.reattached {
        ", re-attached after a restart of the Daemon"
    } else {
        ""
    };
    let _ = writeln!(text, "    service {} {state}{reattached}", service.service);
}

/// A Foreign process of the Worktree, with the ports it holds.
fn foreign_line(text: &mut String, foreign: &ForeignProcess) {
    let (what, ports) = match foreign {
        ForeignProcess::Process { pid, command, ports } => (format!("process {pid} {command}"), ports),
        ForeignProcess::Container {
            name,
            project,
            service,
            ports,
        } => (
            format!("container {name} (compose project {project}, service {service})"),
            ports,
        ),
    };
    let ports = if ports.is_empty() {
        String::new()
    } else {
        format!(" {}", ports_text(ports))
    };
    let _ = writeln!(text, "    foreign {what}{ports}, started outside Procly");
}

/// `port 5432`, `ports 5432, 5433` or `no port`.
pub(crate) fn ports_text(ports: &[u16]) -> String {
    match ports {
        [] => "no port".to_owned(),
        [port] => format!("port {port}"),
        ports => {
            let ports: Vec<String> = ports.iter().map(u16::to_string).collect();
            format!("ports {}", ports.join(", "))
        }
    }
}

/// How a Service ended, after `ended: `.
fn ended(end: ServiceEnd) -> String {
    match end {
        ServiceEnd::Exited { code } => format!("exited with code {code}"),
        ServiceEnd::Signaled { signal } => format!("killed by signal {signal}"),
        ServiceEnd::HealthTimeout { timeout_ms } => format!(
            "not healthy within {}, its process tree was stopped",
            duration::format(Duration::from_millis(timeout_ms))
        ),
        ServiceEnd::Stopped => "stopped by procly down".to_owned(),
        ServiceEnd::Idle { timeout_ms } => format!(
            "stopped after {} idle",
            duration::format(Duration::from_millis(timeout_ms))
        ),
        ServiceEnd::Failed { restarts } => format!("kept crashing, given up after {restarts} restarts"),
        ServiceEnd::PortMismatch { leased, actual } => {
            format!("listened on port {actual} instead of its Lease, port {leased} (Port mismatch)")
        }
        ServiceEnd::Lost => "exit code unknown, a previous Daemon started it".to_owned(),
        ServiceEnd::Orphaned { pgid, port: Some(port) } => format!(
            "its leader ended while no Daemon ran, the rest of process group {pgid} held port {port} of its Lease \
             and was stopped"
        ),
        ServiceEnd::Orphaned { pgid, port: None } => {
            format!("its leader ended while no Daemon ran, the rest of process group {pgid} was stopped")
        }
        ServiceEnd::NotListening { port } => format!(
            "no longer listened on its Lease, port {port}, when the Daemon restarted, its process group was stopped"
        ),
    }
}

/// The directory of a Job is left out when it is the root of its Worktree, and shown relative to it otherwise.
fn job_line(text: &mut String, indent: &str, job: &JobStatus, worktree: &Path) {
    let state = match job.state {
        JobState::Running => "running".to_owned(),
        JobState::Ended {
            end: JobEnd::Exited { code },
        } => format!("exited {code}"),
        JobState::Ended {
            end: JobEnd::Signaled { signal },
        } => format!("killed by signal {signal}"),
        JobState::Ended {
            end: JobEnd::TimedOut { timeout_ms },
        } => format!(
            "timed out after {}",
            duration::format(Duration::from_millis(timeout_ms))
        ),
    };
    let dir = match job.cwd.strip_prefix(worktree) {
        Ok(relative) if relative.as_os_str().is_empty() => String::new(),
        Ok(relative) => format!(" in {}", relative.display()),
        Err(_) => format!(" in {}", job.cwd.display()),
    };
    let _ = writeln!(text, "{indent}job {} {state}{dir}: {}", job.id, job.command);
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::protocol::{RepositoryStatus, WorktreeStatus};
    use crate::topology::Worktree;

    fn job(id: &str, command: &str, cwd: &str, state: JobState) -> JobStatus {
        JobStatus {
            id: id.to_owned(),
            command: command.to_owned(),
            cwd: PathBuf::from(cwd),
            state,
        }
    }

    #[test]
    fn text_nests_worktrees_under_their_repository_and_jobs_under_their_worktree() {
        let report = StatusReport {
            repositories: vec![RepositoryStatus {
                git_common_dir: PathBuf::from("/src/app/.git"),
                worktrees: vec![
                    WorktreeStatus {
                        worktree: Worktree {
                            path: PathBuf::from("/src/app"),
                            slug: "app".to_owned(),
                            main: true,
                        },
                        services: vec![],
                        jobs: vec![
                            job("3f9a1c2e", "sleep 30", "/src/app", JobState::Running),
                            job(
                                "0b1c2d3e",
                                "cargo test",
                                "/src/app/crates/api",
                                JobState::Ended {
                                    end: JobEnd::Exited { code: 101 },
                                },
                            ),
                        ],
                        foreign: vec![
                            ForeignProcess::Process {
                                pid: 4242,
                                command: "node".to_owned(),
                                ports: vec![5173],
                            },
                            ForeignProcess::Container {
                                name: "app-db-1".to_owned(),
                                project: "app".to_owned(),
                                service: "db".to_owned(),
                                ports: vec![5432, 5433],
                            },
                        ],
                    },
                    WorktreeStatus {
                        worktree: Worktree {
                            path: PathBuf::from("/src/app-login"),
                            slug: "app_login".to_owned(),
                            main: false,
                        },
                        services: vec![],
                        jobs: vec![],
                        foreign: vec![],
                    },
                ],
            }],
            jobs_outside_repositories: vec![
                job(
                    "77aa88bb",
                    "make",
                    "/tmp/scratch",
                    JobState::Ended {
                        end: JobEnd::Signaled { signal: 15 },
                    },
                ),
                job(
                    "99cc00dd",
                    "sleep 99",
                    "/tmp/scratch",
                    JobState::Ended {
                        end: JobEnd::TimedOut { timeout_ms: 90_000 },
                    },
                ),
            ],
        };

        assert_eq!(
            text(&report),
            "repository /src/app/.git\n\
             \x20 worktree app (main) /src/app\n\
             \x20   foreign process 4242 node port 5173, started outside Procly\n\
             \x20   foreign container app-db-1 (compose project app, service db) ports 5432, 5433, started outside \
             Procly\n\
             \x20   job 3f9a1c2e running: sleep 30\n\
             \x20   job 0b1c2d3e exited 101 in crates/api: cargo test\n\
             \x20 worktree app_login /src/app-login\n\
             \x20   no jobs\n\
             outside any repository\n\
             \x20 job 77aa88bb killed by signal 15 in /tmp/scratch: make\n\
             \x20 job 99cc00dd timed out after 90s in /tmp/scratch: sleep 99\n"
        );
    }

    #[test]
    fn text_says_so_when_there_is_nothing_to_report() {
        assert_eq!(text(&StatusReport::default()), "no jobs\n");
    }
}
