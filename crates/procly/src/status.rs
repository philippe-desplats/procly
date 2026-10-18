//! The output of `procly status`: compact text by default, versioned JSON on request.

use std::fmt::Write as _;
use std::path::Path;
use std::time::Duration;

use serde::Serialize;

use crate::duration;
use crate::protocol::{JobEnd, JobState, JobStatus, StatusReport};

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

/// One line per Repository, Worktree and Job, indented by nesting.
pub(crate) fn text(report: &StatusReport) -> String {
    let mut text = String::new();
    for repository in &report.repositories {
        let _ = writeln!(text, "repository {}", repository.git_common_dir.display());
        for status in &repository.worktrees {
            let worktree = &status.worktree;
            let main = if worktree.main { " (main)" } else { "" };
            let _ = writeln!(text, "  worktree {}{main} {}", worktree.slug, worktree.path.display());
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
                    },
                    WorktreeStatus {
                        worktree: Worktree {
                            path: PathBuf::from("/src/app-login"),
                            slug: "app_login".to_owned(),
                            main: false,
                        },
                        jobs: vec![],
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
