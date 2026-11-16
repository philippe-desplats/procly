//! The output of `procly status`: compact text by default, versioned JSON on request.

use std::fmt::Write as _;
use std::path::Path;
use std::time::Duration;

use crate::duration;
use crate::protocol::{
    ContainerStatus, ForeignProcess, JobEnd, JobState, JobStatus, ServiceEnd, ServicePorts, ServiceState,
    ServiceStatus, StatusReport,
};

pub(crate) fn json(report: &StatusReport) -> String {
    crate::json::render(report)
        .expect("a status report decoded from JSON holds only UTF-8 paths, so it serializes back")
}

/// One line per Repository, Worktree, Service and Job, indented by nesting.
pub(crate) fn text(report: &StatusReport) -> String {
    let mut text = String::new();
    for repository in &report.repositories {
        let _ = writeln!(text, "repository {}", repository.git_common_dir.display());
        for status in &repository.worktrees {
            let worktree = &status.worktree;
            let main = if worktree.main { " (main)" } else { "" };
            let branch = status
                .branch
                .as_ref()
                .map(|branch| format!(" on {branch}"))
                .unwrap_or_default();
            let removed = if status.listed { "" } else { " (removed)" };
            let _ = writeln!(
                text,
                "  worktree {}{main} {}{branch}{removed}",
                worktree.slug,
                worktree.path.display()
            );
            for service in &status.services {
                service_line(&mut text, service);
            }
            for container in &status.containers {
                container_line(&mut text, container);
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
    let port = reach(service);
    let state = match &service.state {
        ServiceState::Starting => format!("starting{port}"),
        ServiceState::Healthy => format!("healthy{port}"),
        ServiceState::Restarting => format!("restarting{port}"),
        ServiceState::Ended { end } => format!("ended: {}", ended(&service.service, end)),
    };
    let reattached = if service.reattached {
        ", re-attached after a restart of the Daemon"
    } else {
        ""
    };
    let _ = writeln!(text, "    service {} {state}{reattached}", service.service);
    for stale in &service.stale_leases {
        let _ = writeln!(
            text,
            "      Lease {}.{} port {} differs from its declared port {}, it moves there once {} stops",
            stale.service, stale.port_name, stale.port, stale.declared, service.service
        );
    }
}

/// Where a running Service is reached, as `up` names its Leases: ` <url> port <n>` for the Lease its health check
/// probes, ` port <n>` when the scheme of that Lease is not known, then `, <url>` for each other Lease whose scheme
/// is. Empty for a Service without a Lease to probe.
fn reach(service: &ServiceStatus) -> String {
    let Some(port) = service.port else {
        return String::new();
    };
    let probed = |url: &&String| url.rsplit_once(':').and_then(|(_, lease)| lease.parse::<u16>().ok()) == Some(port);
    let mut reach = match service.urls.values().find(probed) {
        Some(url) => format!(" {url} port {port}"),
        None => format!(" port {port}"),
    };
    for url in service.urls.values().filter(|url| !probed(url)) {
        let _ = write!(reach, ", {url}");
    }
    reach
}

/// An Isolated container of the Worktree, with the Lease its health check probes.
fn container_line(text: &mut String, container: &ContainerStatus) {
    let port = container.port.map(|port| format!(" port {port}")).unwrap_or_default();
    let _ = writeln!(
        text,
        "    container of service {} (compose project {}, service {}){port}, Isolated",
        container.service, container.project, container.compose
    );
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

/// The Port mismatch of the Group `group` told per Service it covers, with its Lease and the ports seen for it, then
/// the ports no Service declares. `None` for the Port mismatch of a Service alone, whose pair says it all, or of a
/// Daemon that named no Service.
pub(crate) fn by_service(group: &str, services: &[ServicePorts], unattributed: &[u16]) -> Option<String> {
    if services.iter().all(|service| service.service == group) {
        return None;
    }
    let mut parts: Vec<String> = services
        .iter()
        .map(|ServicePorts { service, leased, seen }| {
            let elsewhere: Vec<u16> = seen.iter().copied().filter(|port| port != leased).collect();
            if seen.contains(leased) {
                format!("{service} on its Lease, port {leased}")
            } else if elsewhere.is_empty() {
                format!("{service} on no port of its own, its Lease is port {leased}")
            } else {
                format!("{service} on {} instead of its Lease, port {leased}", ports(&elsewhere))
            }
        })
        .collect();
    if !unattributed.is_empty() {
        parts.push(format!("and {} that no Service declares", ports(unattributed)));
    }
    Some(parts.join(", "))
}

/// `port 3000`, `ports 3000 and 3001`, `ports 3000, 3001 and 3002`.
fn ports(ports: &[u16]) -> String {
    match ports {
        [] => String::new(),
        [port] => format!("port {port}"),
        [rest @ .., last] => format!(
            "ports {} and {last}",
            rest.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")
        ),
    }
}

/// How the Service `service` ended, after `ended: `.
fn ended(service: &str, end: &ServiceEnd) -> String {
    match end {
        ServiceEnd::Exited { code } => format!("exited with code {code}"),
        ServiceEnd::Signaled { signal } => format!("killed by signal {signal}"),
        ServiceEnd::HealthTimeout { timeout_ms } => format!(
            "not healthy within {}, its process tree was stopped",
            duration::format(Duration::from_millis(*timeout_ms))
        ),
        ServiceEnd::Stopped => "stopped by procly down".to_owned(),
        ServiceEnd::Idle { timeout_ms } => format!(
            "stopped after {} idle",
            duration::format(Duration::from_millis(*timeout_ms))
        ),
        ServiceEnd::Failed { restarts } => format!("kept crashing, given up after {restarts} restarts"),
        ServiceEnd::PortMismatch {
            leased,
            actual,
            services,
            unattributed,
        } => match by_service(service, services, unattributed) {
            Some(by_service) => {
                format!(
                    "listened on ports other than the Leases of the Services it covers (Port mismatch): {by_service}"
                )
            }
            None => format!("listened on port {actual} instead of its Lease, port {leased} (Port mismatch)"),
        },
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
                        branch: Some("feature/login".to_owned()),
                        listed: true,
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
                        containers: vec![],
                    },
                    WorktreeStatus {
                        worktree: Worktree {
                            path: PathBuf::from("/src/app-login"),
                            slug: "app_login".to_owned(),
                            main: false,
                        },
                        branch: None,
                        listed: true,
                        services: vec![],
                        jobs: vec![],
                        foreign: vec![],
                        containers: vec![ContainerStatus {
                            service: "cache".to_owned(),
                            project: "app-app_login".to_owned(),
                            compose: "redis".to_owned(),
                            port: Some(24_001),
                        }],
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
             \x20 worktree app (main) /src/app on feature/login\n\
             \x20   foreign process 4242 node port 5173, started outside Procly\n\
             \x20   foreign container app-db-1 (compose project app, service db) ports 5432, 5433, started outside \
             Procly\n\
             \x20   job 3f9a1c2e running: sleep 30\n\
             \x20   job 0b1c2d3e exited 101 in crates/api: cargo test\n\
             \x20 worktree app_login /src/app-login\n\
             \x20   container of service cache (compose project app-app_login, service redis) port 24001, Isolated\n\
             \x20   no jobs\n\
             outside any repository\n\
             \x20 job 77aa88bb killed by signal 15 in /tmp/scratch: make\n\
             \x20 job 99cc00dd timed out after 90s in /tmp/scratch: sleep 99\n"
        );
    }

    #[test]
    fn text_marks_a_worktree_git_no_longer_lists_as_removed() {
        let report = StatusReport {
            repositories: vec![RepositoryStatus {
                git_common_dir: PathBuf::from("/src/app/.git"),
                worktrees: vec![WorktreeStatus {
                    worktree: Worktree {
                        path: PathBuf::from("/src/app-gone"),
                        slug: "app_gone".to_owned(),
                        main: false,
                    },
                    branch: None,
                    listed: false,
                    services: vec![],
                    jobs: vec![],
                    foreign: vec![],
                    containers: vec![],
                }],
            }],
            jobs_outside_repositories: vec![],
        };

        assert_eq!(
            text(&report),
            "repository /src/app/.git\n  worktree app_gone /src/app-gone (removed)\n    no jobs\n"
        );
    }

    #[test]
    fn text_says_so_when_there_is_nothing_to_report() {
        assert_eq!(text(&StatusReport::default()), "no jobs\n");
    }

    fn running(port: Option<u16>, urls: &[(&str, &str)]) -> ServiceStatus {
        ServiceStatus {
            service: "mailpit".to_owned(),
            port,
            reattached: false,
            started_at: 0,
            restartable: false,
            stale_leases: Vec::new(),
            urls: urls
                .iter()
                .map(|&(port_name, url)| (port_name.to_owned(), url.to_owned()))
                .collect(),
            state: ServiceState::Healthy,
        }
    }

    #[test]
    fn a_running_service_shows_the_url_of_its_probed_lease_then_the_urls_of_the_others() {
        let line = |service: &ServiceStatus| {
            let mut text = String::new();
            service_line(&mut text, service);
            text
        };

        let mailpit = running(
            Some(1025),
            &[("http", "http://localhost:8025"), ("smtp", "smtp://localhost:1025")],
        );
        assert_eq!(
            line(&mailpit),
            "    service mailpit healthy smtp://localhost:1025 port 1025, http://localhost:8025\n"
        );
        assert_eq!(
            line(&running(Some(4000), &[])),
            "    service mailpit healthy port 4000\n"
        );
        assert_eq!(line(&running(None, &[])), "    service mailpit healthy\n");
    }

    fn ports_of(service: &str, leased: u16, seen: &[u16]) -> ServicePorts {
        ServicePorts {
            service: service.to_owned(),
            leased,
            seen: seen.to_vec(),
        }
    }

    #[test]
    fn a_group_mismatch_names_each_service_where_it_was_seen_then_the_ports_no_service_declares() {
        let services = [
            ports_of("api", 24004, &[3000, 3002]),
            ports_of("web", 24005, &[24005]),
            ports_of("docs", 24006, &[]),
        ];

        assert_eq!(
            by_service("dev", &services, &[9229]).as_deref(),
            Some(
                "api on ports 3000 and 3002 instead of its Lease, port 24004, web on its Lease, port 24005, docs on \
                 no port of its own, its Lease is port 24006, and port 9229 that no Service declares"
            )
        );
    }

    #[test]
    fn the_mismatch_of_a_service_alone_or_of_a_daemon_that_named_none_is_told_by_its_pair() {
        assert_eq!(by_service("web", &[ports_of("web", 24004, &[3000])], &[]), None);
        assert_eq!(by_service("web", &[], &[9229]), None);
    }
}
