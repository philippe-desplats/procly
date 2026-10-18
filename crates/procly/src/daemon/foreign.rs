//! Foreign processes: what runs in a Worktree, or holds what a Service needs, without Procly having started it.
//!
//! Procly only names them, with the command that stops them. It never signals or stops one itself.

use std::collections::BTreeSet;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use super::listeners;
use crate::protocol::{ForeignProcess, Occupant};
use crate::topology;

/// How long a probe waits for a connection to a port.
const CONNECT_TIMEOUT: Duration = Duration::from_millis(200);
/// The label compose gives the containers of a project with the directory of that project.
const WORKING_DIR_LABEL: &str = "com.docker.compose.project.working_dir";
/// What `docker ps` prints for each container, one field per tab.
const PS_FORMAT: &str = "{{.Names}}\t{{.Label \"com.docker.compose.project\"}}\t{{.Label \"com.docker.compose.service\"}}\t\
                         {{.Label \"com.docker.compose.oneoff\"}}\t{{.Label \"com.docker.compose.project.working_dir\"}}\t\
                         {{.State}}\t{{.Ports}}";
/// The processes of the Docker engine that forward published ports, which Procly never tells anyone to kill: the
/// container publishing the port is what to stop.
const ENGINE_COMMANDS: [&str; 5] = ["com.docker", "vpnkit", "docker-proxy", "dockerd", "containerd"];

/// A container as `docker ps` lists it, with its compose labels when compose created it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Container {
    pub(super) name: String,
    pub(super) project: Option<String>,
    pub(super) service: Option<String>,
    /// Created by `docker compose run`, which is never the container of the service itself.
    pub(super) oneoff: bool,
    pub(super) working_dir: Option<PathBuf>,
    pub(super) running: bool,
    /// The ports it publishes on the host, sorted, each once.
    pub(super) ports: Vec<u16>,
}

impl Container {
    /// Whether compose created it as the container of the service `service` of the project `project`.
    pub(super) fn is_service(&self, project: &str, service: &str) -> bool {
        !self.oneoff && self.project.as_deref() == Some(project) && self.service.as_deref() == Some(service)
    }

    pub(super) fn occupant(&self) -> Occupant {
        Occupant::Container {
            name: self.name.clone(),
            project: self.project.clone(),
            service: self.service.clone(),
        }
    }
}

/// The containers `docker ps` lists with the filters `args`, running `docker` in `env` when given and in the
/// environment of the Daemon otherwise. `None` when Docker cannot tell, as when it is not installed or not running.
/// Blocking.
pub(super) fn containers(args: &[&str], env: Option<&[(String, String)]>) -> Option<Vec<Container>> {
    let mut command = Command::new("docker");
    command
        .arg("ps")
        .args(args)
        .args(["--no-trunc", "--format", PS_FORMAT])
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    if let Some(env) = env {
        command.env_clear().envs(env.iter().map(|(name, value)| (name, value)));
    }
    let output = command.output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(parse_container)
            .collect(),
    )
}

/// What holds `port`, with the command that stops it, `None` when nothing answers on it. Blocking.
///
/// A container publishing the port is named before any process, since the process `lsof` finds for it is the
/// Docker engine, which must keep running.
/// Docker runs in `env` when given, as for [`containers`].
pub(super) fn occupant(port: u16, env: Option<&[(String, String)]>) -> Option<(Occupant, Option<String>)> {
    if !answers(port) {
        return None;
    }
    let filter = format!("publish={port}");
    if let Some(container) = containers(&["--filter", &filter], env).and_then(|found| found.into_iter().next()) {
        let stop = format!("docker stop {}", container.name);
        return Some((container.occupant(), Some(stop)));
    }
    let process = listeners::listeners(Some(port))
        .unwrap_or_default()
        .into_iter()
        .find(|listener| !is_engine(&listener.command));
    Some(match process {
        Some(process) => (
            Occupant::Process {
                pid: process.pid,
                command: process.command,
            },
            Some(format!("kill {}", process.pid)),
        ),
        None => (Occupant::Unknown, None),
    })
}

/// Whether a running container of the compose service `service` whose project lives in `main` publishes `port`,
/// so that its Lease is the port that container already holds and Adoption finds it there. Blocking.
pub(super) fn published_by_service(port: u16, main: &Path, service: &str) -> bool {
    let filter = format!("publish={port}");
    containers(&["--filter", &filter], None)
        .unwrap_or_default()
        .iter()
        .any(|container| {
            !container.oneoff
                && container.service.as_deref() == Some(service)
                && container.working_dir.as_deref().map(canonical).as_deref() == Some(main)
        })
}

/// The Foreign processes of each of `worktrees`, in their order: the processes listening on TCP whose working
/// directory is in the Worktree, and the running containers of compose projects whose directory is. A process of a
/// group in `owned_groups` and a container of a project and service in `owned_containers` are Procly's. Blocking.
pub(super) fn in_worktrees(
    worktrees: &[PathBuf],
    owned_groups: &BTreeSet<i32>,
    owned_containers: &BTreeSet<(String, String)>,
) -> Vec<Vec<ForeignProcess>> {
    let mut foreign = vec![Vec::new(); worktrees.len()];
    let daemon = i32::try_from(std::process::id()).unwrap_or_default();
    let daemon_group = nix::unistd::getpgrp().as_raw();
    let listening: Vec<_> = listeners::listeners(None)
        .unwrap_or_default()
        .into_iter()
        .filter(|listener| {
            listener.pid != daemon && listener.pgid != daemon_group && !owned_groups.contains(&listener.pgid)
        })
        .collect();
    let pids: Vec<i32> = listening.iter().map(|listener| listener.pid).collect();
    let directories = listeners::working_directories(&pids);
    for listener in listening {
        let Some(directory) = directories.get(&listener.pid) else {
            continue;
        };
        if let Some(index) = deepest(worktrees, &canonical(directory)) {
            foreign[index].push(ForeignProcess::Process {
                pid: listener.pid,
                command: listener.command,
                ports: listener.ports,
            });
        }
    }
    let label = format!("label={WORKING_DIR_LABEL}");
    for container in containers(&["--filter", &label], None).unwrap_or_default() {
        let (Some(project), Some(service), Some(working_dir)) =
            (&container.project, &container.service, &container.working_dir)
        else {
            continue;
        };
        if owned_containers.contains(&(project.clone(), service.clone())) {
            continue;
        }
        if let Some(index) = deepest(worktrees, &canonical(working_dir)) {
            foreign[index].push(ForeignProcess::Container {
                name: container.name.clone(),
                project: project.clone(),
                service: service.clone(),
                ports: container.ports.clone(),
            });
        }
    }
    foreign
}

/// Whether something accepts connections on `port` of the loopback, IPv4 or IPv6. Unlike a bind, a connection
/// left in `TIME_WAIT` by a process that has ended does not count.
fn answers(port: u16) -> bool {
    [
        SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
        SocketAddr::from((Ipv6Addr::LOCALHOST, port)),
    ]
    .iter()
    .any(|address| TcpStream::connect_timeout(address, CONNECT_TIMEOUT).is_ok())
}

fn is_engine(command: &str) -> bool {
    ENGINE_COMMANDS.iter().any(|engine| command.starts_with(engine))
}

/// The index of the Worktree of `worktrees` holding `path`, the deepest one when Worktrees nest.
fn deepest(worktrees: &[PathBuf], path: &Path) -> Option<usize> {
    worktrees
        .iter()
        .enumerate()
        .filter(|(_, worktree)| path.starts_with(worktree))
        .max_by_key(|(_, worktree)| worktree.components().count())
        .map(|(index, _)| index)
}

/// `path` with its symbolic links resolved, as Worktree roots are, or as it is when it cannot be resolved.
fn canonical(path: &Path) -> PathBuf {
    topology::canonicalize(path).unwrap_or_else(|_| path.to_owned())
}

/// One line of `docker ps` in [`PS_FORMAT`].
fn parse_container(line: &str) -> Option<Container> {
    let fields: Vec<&str> = line.split('\t').collect();
    let [name, project, service, oneoff, working_dir, state, ports] = fields[..] else {
        return None;
    };
    let label = |value: &str| (!value.is_empty()).then(|| value.to_owned());
    Some(Container {
        name: name.to_owned(),
        project: label(project),
        service: label(service),
        oneoff: oneoff.eq_ignore_ascii_case("true"),
        working_dir: label(working_dir).map(PathBuf::from),
        running: state == "running",
        ports: published_ports(ports),
    })
}

/// The host ports of the `Ports` column of `docker ps`, such as `0.0.0.0:5432->5432/tcp, [::]:5432->5432/tcp` or
/// `0.0.0.0:8000-8001->80-81/tcp`. A port the container exposes without publishing it, such as `80/tcp`, is left out.
fn published_ports(text: &str) -> Vec<u16> {
    let mut ports: Vec<u16> = text
        .split(", ")
        .filter_map(|mapping| mapping.split_once("->"))
        .filter_map(|(host, _)| host.rsplit(':').next())
        .flat_map(|published| match published.split_once('-') {
            Some((first, last)) => match (first.parse::<u16>(), last.parse::<u16>()) {
                (Ok(first), Ok(last)) => (first..=last).collect(),
                _ => Vec::new(),
            },
            None => published.parse().into_iter().collect(),
        })
        .collect();
    ports.sort_unstable();
    ports.dedup();
    ports
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_host_ports_a_container_publishes_are_read_once_each() {
        assert_eq!(
            published_ports("0.0.0.0:5432->5432/tcp, [::]:5432->5432/tcp"),
            [5432]
        );
        assert_eq!(published_ports("0.0.0.0:8000-8002->80-82/tcp"), [8000, 8001, 8002]);
        assert_eq!(published_ports("80/tcp"), Vec::<u16>::new());
        assert_eq!(published_ports(""), Vec::<u16>::new());
    }

    #[test]
    fn a_container_line_carries_its_compose_labels_when_it_has_them() {
        let line = "app-db-1\tapp\tdb\tFalse\t/src/app\trunning\t0.0.0.0:24000->80/tcp";
        let container = parse_container(line).unwrap();
        assert!(container.is_service("app", "db"));
        assert!(!container.is_service("app", "web"));
        assert_eq!(container.ports, [24000]);
        assert_eq!(container.working_dir, Some(PathBuf::from("/src/app")));

        let plain = parse_container("occupant\t\t\t\t\texited\t").unwrap();
        assert_eq!(plain.project, None);
        assert!(!plain.running);
        assert!(!plain.is_service("app", "db"));
    }

    #[test]
    fn a_one_off_container_of_compose_run_is_not_the_container_of_its_service() {
        let line = "app-db-run-1\tapp\tdb\tTrue\t/src/app\trunning\t";
        assert!(!parse_container(line).unwrap().is_service("app", "db"));
    }

    #[test]
    fn a_path_belongs_to_the_deepest_worktree_holding_it() {
        let worktrees = [PathBuf::from("/src/app"), PathBuf::from("/src/app/.worktrees/feature")];
        assert_eq!(deepest(&worktrees, Path::new("/src/app/web")), Some(0));
        assert_eq!(deepest(&worktrees, Path::new("/src/app/.worktrees/feature/web")), Some(1));
        assert_eq!(deepest(&worktrees, Path::new("/src/other")), None);
    }
}
