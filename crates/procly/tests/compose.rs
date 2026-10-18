//! Compose services in Shared mode. The tests marked Docker drive a real engine and skip themselves when
//! `docker compose` v2 is unavailable, they start containers of projects named by the test and remove them at the end.

mod common;

use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use common::{
    Procly, assert_success, eventually, free_port, git, git_repository, git_worktree_add, require_docker, stderr,
    stdout, test_server,
};

/// A compose file running a TCP listener in `alpine`, which is small and carries a `nc` that serves many clients.
/// The port comes from `DB_PORT`, and the data lives in a named volume.
fn compose_file(project: &str) -> String {
    format!(
        "name: {project}\nservices:\n  db:\n    image: alpine\n    init: true\n    command: [\"nc\", \"-lk\", \"-p\", \"80\", \"-e\", \"/bin/cat\"]\n    \
         ports:\n      - \"${{DB_PORT:-1}}:80\"\n    volumes:\n      - data:/data\nvolumes:\n  data:\n"
    )
}

/// The `procly.toml` of a Repository whose service `db` is the compose service `db`.
fn config(port: u16, extra: &str) -> String {
    format!(
        "[services.db]\ncompose = \"db\"\nports = {{ db = {port} }}\nenv = {{ DB_PORT = \"{{services.db.ports.db}}\" }}\n{extra}"
    )
}

/// Removes the containers, network and volumes of a test project when the test ends, passed or failed.
struct Project {
    name: String,
    dir: PathBuf,
}

impl Project {
    fn try_docker(&self, args: &[&str]) -> std::io::Result<std::process::Output> {
        Command::new("docker")
            .args(["compose", "-p", &self.name])
            .args(args)
            .current_dir(&self.dir)
            .env("DB_PORT", "1")
            .output()
    }

    fn docker(&self, args: &[&str]) -> std::process::Output {
        self.try_docker(args).expect("docker runs")
    }

    /// The id of the container of service `db`, running or not, empty when there is none.
    fn container(&self) -> String {
        stdout(&self.docker(&["ps", "-a", "-q", "db"])).trim().to_owned()
    }

    fn is_running(&self) -> bool {
        stdout(&self.docker(&["ps", "--status", "running", "--services"]))
            .lines()
            .any(|service| service == "db")
    }

    fn volume_exists(&self) -> bool {
        Command::new("docker")
            .args(["volume", "inspect", &format!("{}_data", self.name)])
            .output()
            .expect("docker runs")
            .status
            .success()
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        // A test that needs no Docker never started anything, and may run where Docker is absent.
        let _ = self.try_docker(&["down", "--volumes", "--remove-orphans"]);
    }
}

/// A Repository with a compose file, a `procly.toml` and a second Worktree, in a project of its own.
struct Fixture {
    // First, so that the containers are removed while the working directories still exist.
    project: Project,
    procly: Procly,
    main: PathBuf,
    other: PathBuf,
    port: u16,
}

fn fixture(extra: &str) -> Fixture {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let procly = Procly::with_clock();
    let name = format!("pcl-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
    let main = git_repository(procly.scratch(), "app");
    let port = free_port();
    std::fs::write(main.join("compose.yaml"), compose_file(&name)).expect("write the compose file");
    std::fs::write(main.join("procly.toml"), config(port, extra)).expect("write the config");
    git(&main, &["add", "."]);
    git(&main, &["commit", "--quiet", "--message", "compose"]);
    let other = git_worktree_add(&main, "app-feature");
    Fixture {
        procly,
        project: Project {
            name,
            dir: main.clone(),
        },
        main,
        other,
        port,
    }
}

fn listens(port: u16) -> bool {
    TcpStream::connect(("127.0.0.1", port)).is_ok()
}

#[test]
fn docker_up_starts_the_service_through_the_unmodified_compose_file_on_its_lease() {
    if !require_docker() {
        return;
    }
    let fixture = fixture("");
    let compose_before = std::fs::read(fixture.main.join("compose.yaml")).unwrap();

    let output = fixture.procly.procly_in(&fixture.main, &["up", "db"]);

    assert_success(&output);
    let port = fixture.port;
    assert_eq!(
        stdout(&output),
        format!("db started http://localhost:{port} port {port}\n")
    );
    // The compose file read `DB_PORT`, so the container is reachable on the Lease and nowhere else.
    assert!(listens(port), "nothing listens on {port}");
    assert_eq!(
        std::fs::read(fixture.main.join("compose.yaml")).unwrap(),
        compose_before
    );
    assert!(
        stdout(&fixture.procly.procly_in(&fixture.main, &["up", "db"])).contains("db reused"),
        "the second up did not reuse the running container"
    );
}

#[test]
fn docker_2_worktrees_share_one_container_on_one_port() {
    if !require_docker() {
        return;
    }
    let fixture = fixture("");
    assert_success(&fixture.procly.procly_in(&fixture.main, &["up", "db"]));
    let container = fixture.project.container();
    assert!(!container.is_empty(), "no container after up");

    let output = fixture.procly.procly_in(&fixture.other, &["up", "db"]);

    assert_success(&output);
    let port = fixture.port;
    assert_eq!(
        stdout(&output),
        format!("db reused http://localhost:{port} port {port}\n")
    );
    // The same container, not one recreated under the first Worktree.
    assert_eq!(fixture.project.container(), container);
    assert!(fixture.project.is_running());
}

#[test]
fn docker_the_container_stops_after_the_grace_period_once_no_worktree_needs_it_and_keeps_its_volume() {
    if !require_docker() {
        return;
    }
    let fixture = fixture("grace_period = \"1m\"\n");
    assert_success(&fixture.procly.procly_in(&fixture.main, &["up", "db"]));
    assert_success(&fixture.procly.procly_in(&fixture.other, &["up", "db"]));
    let container = fixture.project.container();

    let first = fixture.procly.procly_in(&fixture.main, &["down", "db"]);
    assert_success(&first);
    assert_eq!(stdout(&first), "db released, 1 other Worktree still needs it\n");
    // One Worktree still needs it, however long the others waited.
    fixture.procly.advance_clock(Duration::from_mins(10));
    assert!(fixture.project.is_running());

    let last = fixture.procly.procly_in(&fixture.other, &["down", "db"]);
    assert_success(&last);
    assert_eq!(
        stdout(&last),
        "db released, it stops after 1m unless a Worktree needs it again\n"
    );
    fixture.procly.advance_clock(Duration::from_secs(59));
    assert!(fixture.project.is_running(), "stopped before the Grace period ended");

    fixture.procly.advance_clock(Duration::from_secs(2));
    eventually("the container to stop after the Grace period", || {
        !fixture.project.is_running()
    });
    // Stopped, never removed, and the data stays.
    assert_eq!(fixture.project.container(), container);
    assert!(fixture.project.volume_exists());
    assert!(!listens(fixture.port));
}

#[test]
fn docker_an_up_during_the_grace_period_keeps_the_container_running() {
    if !require_docker() {
        return;
    }
    let fixture = fixture("grace_period = \"1m\"\n");
    assert_success(&fixture.procly.procly_in(&fixture.main, &["up", "db"]));
    assert_success(&fixture.procly.procly_in(&fixture.main, &["down", "db"]));
    fixture.procly.advance_clock(Duration::from_secs(30));

    let output = fixture.procly.procly_in(&fixture.other, &["up", "db"]);

    assert_success(&output);
    assert!(stdout(&output).starts_with("db reused "));
    // Well past the Grace period the first release started, a Worktree needs it again.
    fixture.procly.advance_clock(Duration::from_mins(10));
    std::thread::sleep(Duration::from_millis(500));
    assert!(fixture.project.is_running());
    assert!(listens(fixture.port));
}

#[test]
fn docker_a_process_service_starts_the_compose_service_it_needs_first() {
    if !require_docker() {
        return;
    }
    let fixture = fixture("");
    let web_port = free_port();
    std::fs::write(
        fixture.main.join("procly.local.toml"),
        format!(
            "[services.web]\nrun = \"{} --port {{ports.http}}\"\nports = {{ http = {web_port} }}\nneeds = [\"db\"]\n",
            test_server().display()
        ),
    )
    .expect("write the config");

    let output = fixture.procly.procly_in(&fixture.main, &["up", "web"]);

    assert_success(&output);
    assert_eq!(
        stdout(&output),
        format!("web started http://localhost:{web_port} port {web_port}\n")
    );
    let port = fixture.port;
    assert_eq!(
        stderr(&output),
        format!("db started http://localhost:{port} port {port} (needed by web)\n")
    );
    assert!(fixture.project.is_running());
}

#[test]
fn docker_down_without_a_service_releases_every_compose_service_of_the_worktree() {
    if !require_docker() {
        return;
    }
    let fixture = fixture("");
    assert_success(&fixture.procly.procly_in(&fixture.main, &["up", "db"]));

    let output = fixture.procly.procly_in(&fixture.main, &["down"]);

    assert_success(&output);
    assert_eq!(
        stdout(&output),
        "db released, it stops after 10m unless a Worktree needs it again\n"
    );
    let again = fixture.procly.procly_in(&fixture.main, &["down", "db"]);
    assert_eq!(stdout(&again), "db not running\n");
}

#[test]
fn docker_a_compose_file_that_does_not_publish_the_lease_is_refused() {
    if !require_docker() {
        return;
    }
    let fixture = fixture("");
    // The compose file interpolates `DB_PORT`, which the config sets to anything but the Lease.
    std::fs::write(
        fixture.main.join("procly.local.toml"),
        "[services.db.env]\nDB_PORT = \"1\"\n",
    )
    .unwrap();

    let output = fixture.procly.procly_in(&fixture.main, &["up", "db"]);

    assert_eq!(output.status.code(), Some(125), "{}", stderr(&output));
    let message = stderr(&output);
    assert!(message.contains("does not publish"), "{message}");
    assert!(message.contains("PORT_DB"), "{message}");
    assert!(fixture.project.container().is_empty(), "a container was created");
}

#[test]
fn docker_a_service_missing_from_the_compose_file_is_refused() {
    if !require_docker() {
        return;
    }
    let fixture = fixture("");
    std::fs::write(
        fixture.main.join("procly.local.toml"),
        "[services.db]\ncompose = \"missing\"\n",
    )
    .unwrap();

    let output = fixture.procly.procly_in(&fixture.main, &["up", "db"]);

    assert_eq!(output.status.code(), Some(125));
    assert!(stderr(&output).contains("no service missing in the compose file"));
}

/// Runs `procly up db` in a Repository without Docker, with `bin` as the only directory docker may be found in.
fn up_with_path(path: &str, setup: impl FnOnce(&Path)) -> std::process::Output {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    std::fs::write(main.join("procly.toml"), config(free_port(), "")).expect("write the config");
    let bin = procly.scratch().join("bin");
    std::fs::create_dir(&bin).expect("create the bin directory");
    setup(&bin);
    // The Daemon takes the PATH of the command that starts it, and needs git.
    assert_success(&procly.procly_in(&main, &["status"]));
    procly
        .command()
        .current_dir(&main)
        .env("PATH", path.replace("{bin}", &bin.to_string_lossy()))
        .args(["up", "db"])
        .output()
        .expect("the procly binary runs")
}

#[test]
fn every_worktree_sees_the_leases_of_the_main_worktree_for_a_compose_service() {
    let fixture = fixture("");
    let port = fixture.port;

    for dir in [&fixture.main, &fixture.other] {
        let output = fixture.procly.procly_in(dir, &["env", "db"]);

        assert_success(&output);
        let exports = stdout(&output);
        assert!(exports.contains(&format!("DB_PORT='{port}'")), "{exports}");
        assert!(exports.contains(&format!("PORT_DB='{port}'")), "{exports}");
    }
}

#[test]
fn up_without_docker_says_what_to_install() {
    let output = up_with_path("{bin}", |_| {});

    assert_eq!(output.status.code(), Some(125));
    let message = stderr(&output);
    assert!(
        message.contains("docker compose v2 is required by service db"),
        "{message}"
    );
    assert!(message.contains("docker is not on the PATH"), "{message}");
    assert!(
        message.contains("https://docs.docker.com/compose/install/"),
        "{message}"
    );
}

#[test]
fn up_with_docker_but_without_compose_v2_says_what_to_install() {
    let output = up_with_path("{bin}:/usr/bin:/bin", |bin| {
        let docker = bin.join("docker");
        std::fs::write(
            &docker,
            "#!/bin/sh\necho \"docker: 'compose' is not a docker command.\" >&2\nexit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&docker, std::fs::Permissions::from_mode(0o755)).unwrap();
    });

    assert_eq!(output.status.code(), Some(125));
    let message = stderr(&output);
    assert!(
        message.contains("docker compose v2 is required by service db"),
        "{message}"
    );
    assert!(message.contains("'compose' is not a docker command"), "{message}");
}

#[test]
fn a_service_that_is_both_a_process_and_a_compose_service_is_refused() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    std::fs::write(
        main.join("procly.toml"),
        "[services.db]\nrun = \"true\"\ncompose = \"db\"\n",
    )
    .unwrap();

    let output = procly.procly_in(&main, &["up", "db"]);

    assert_eq!(output.status.code(), Some(125));
    assert!(stderr(&output).contains("exactly one of `run`"), "{}", stderr(&output));
}
