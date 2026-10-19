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
fn docker_the_lease_of_a_running_container_does_not_expire() {
    if !require_docker() {
        return;
    }
    let fixture = fixture("");
    let port = fixture.port;
    assert_success(&fixture.procly.procly_in(&fixture.main, &["up", "db"]));

    // Past the 30 days a Lease no Service uses is kept, while the container runs all along.
    fixture.procly.advance_clock(Duration::from_hours(31 * 24));
    // The declared port moves, so that a Lease that had expired would be granted again on the new one.
    std::fs::write(fixture.main.join("procly.toml"), config(free_port(), "")).expect("write the config");
    let output = fixture.procly.procly_in(&fixture.main, &["env", "db"]);

    assert_success(&output);
    assert!(
        stdout(&output).contains(&format!("export DB_PORT='{port}'\n")),
        "{}",
        stdout(&output)
    );
    assert!(fixture.project.is_running());
    assert!(listens(port));
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
fn a_pinned_branch_does_not_move_the_leases_of_a_compose_service() {
    let pinned = free_port();
    // `app-feature` is the branch git creates for the second Worktree.
    let fixture = fixture(&format!("[pinned.app-feature.ports]\ndb = {{ db = {pinned} }}\n"));
    let port = fixture.port;

    let output = fixture.procly.procly_in(&fixture.other, &["env", "db"]);

    assert_success(&output);
    let exports = stdout(&output);
    assert!(exports.contains(&format!("PORT_DB='{port}'")), "{exports}");
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

/// Starts the service `db` of the project with plain `docker compose`, as an agent bypassing Procly would, publishing
/// it on `port`.
fn compose_up_outside_procly(fixture: &Fixture, port: u16) {
    let output = Command::new("docker")
        .args(["compose", "up", "--detach", "--wait", "db"])
        .current_dir(&fixture.main)
        .env("DB_PORT", port.to_string())
        .output()
        .expect("docker runs");
    assert_success(&output);
}

/// A container started with plain `docker run`, removed when the test ends, passed or failed.
struct Foreign {
    name: String,
}

impl Foreign {
    /// Runs a container named `name` that does nothing, publishing `port` when given.
    fn run(name: &str, port: Option<u16>) -> Self {
        let mut args = vec![
            "run".to_owned(),
            "--detach".to_owned(),
            "--name".to_owned(),
            name.to_owned(),
        ];
        if let Some(port) = port {
            args.extend(["--publish".to_owned(), format!("{port}:80")]);
        }
        args.extend(["alpine".to_owned(), "sleep".to_owned(), "600".to_owned()]);
        let output = Command::new("docker").args(&args).output().expect("docker runs");
        assert_success(&output);
        Self { name: name.to_owned() }
    }

    fn is_running(&self) -> bool {
        let output = Command::new("docker")
            .args(["ps", "--quiet", "--filter", &format!("name=^{}$", self.name)])
            .output()
            .expect("docker runs");
        !stdout(&output).trim().is_empty()
    }
}

impl Drop for Foreign {
    fn drop(&mut self) {
        let _ = Command::new("docker").args(["rm", "--force", &self.name]).output();
    }
}

/// Runs the stop command a conflict printed, which follows `stop it with ` up to the closing backtick.
fn run_stop_command(stderr: &str) {
    let command = stderr
        .split("with `")
        .nth(1)
        .and_then(|rest| rest.split('`').next())
        .unwrap_or_else(|| panic!("no stop command in {stderr}"));
    let output = Command::new("sh").args(["-c", command]).output().expect("sh runs");
    assert_success(&output);
}

#[test]
fn docker_a_container_started_by_plain_docker_compose_with_matching_labels_is_adopted_as_reused() {
    if !require_docker() {
        return;
    }
    let fixture = fixture("");
    let port = fixture.port;
    compose_up_outside_procly(&fixture, port);
    let container = fixture.project.container();

    let output = fixture.procly.procly_in(&fixture.main, &["up", "db"]);

    assert_success(&output);
    assert_eq!(
        stdout(&output),
        format!("db reused http://localhost:{port} port {port}\n")
    );
    // The very container started outside Procly, neither recreated nor restarted.
    assert_eq!(fixture.project.container(), container);
    assert!(fixture.project.is_running());
    let env = stdout(&fixture.procly.procly_in(&fixture.other, &["env", "db"]));
    assert!(env.contains(&format!("export DB_PORT='{port}'\n")), "{env}");
}

#[test]
fn docker_a_container_holding_the_lease_without_matching_labels_is_a_conflict_procly_never_stops() {
    if !require_docker() {
        return;
    }
    let fixture = fixture("");
    let port = fixture.port;
    // Granted first, so that the Lease is the port the other container takes.
    assert_success(&fixture.procly.procly_in(&fixture.main, &["env", "db"]));
    let foreign = Foreign::run(&format!("{}-occupant", fixture.project.name), Some(port));

    let output = fixture.procly.procly_in(&fixture.main, &["up", "db"]);

    assert_eq!(output.status.code(), Some(5), "stderr: {}", stderr(&output));
    let error = stderr(&output);
    assert!(
        error.contains(&format!(
            "port {port} of its Lease is held by container {} started outside Procly, stop it with `docker stop {}`",
            foreign.name, foreign.name
        )),
        "{error}"
    );
    assert!(foreign.is_running(), "Procly stopped a container it does not own");
    assert!(
        fixture.project.container().is_empty(),
        "Procly created the container anyway"
    );

    run_stop_command(&error);
    assert_success(&fixture.procly.procly_in(&fixture.main, &["up", "db"]));
}

/// A test server started by the test itself, outside Procly, killed when the test ends, passed or failed.
struct Outside(std::process::Child);

impl Outside {
    fn listen(port: u16) -> Self {
        let child = Command::new(test_server())
            .args(["--port", &port.to_string()])
            .stdout(std::process::Stdio::null())
            .spawn()
            .expect("the test server starts");
        let outside = Self(child);
        eventually("the test server to listen", || listens(port));
        outside
    }
}

impl Drop for Outside {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn docker_a_process_holding_the_lease_is_a_conflict_naming_the_kill_command_procly_never_runs() {
    if !require_docker() {
        return;
    }
    let fixture = fixture("");
    let port = fixture.port;
    // Granted first, so that the Lease is the port the process takes.
    assert_success(&fixture.procly.procly_in(&fixture.main, &["env", "db"]));
    let outside = Outside::listen(port);
    let pid = outside.0.id();

    let output = fixture.procly.procly_in(&fixture.main, &["up", "db"]);

    assert_eq!(output.status.code(), Some(5), "stderr: {}", stderr(&output));
    let error = stderr(&output);
    assert!(
        error.contains(&format!(
            "port {port} of its Lease is held by process {pid} procly-test-server started outside Procly, stop it \
             with `kill {pid}`"
        )),
        "{error}"
    );
    assert!(listens(port), "Procly stopped a process it did not start");
    assert!(
        fixture.project.container().is_empty(),
        "Procly created the container anyway"
    );

    run_stop_command(&error);
    assert_success(&fixture.procly.procly_in(&fixture.main, &["up", "db"]));
}

#[test]
fn docker_up_json_of_a_conflict_carries_the_conflicts_and_exits_5_as_the_text_does() {
    if !require_docker() {
        return;
    }
    let fixture = fixture("");
    let port = fixture.port;
    assert_success(&fixture.procly.procly_in(&fixture.main, &["env", "db"]));
    let foreign = Foreign::run(&format!("{}-occupant", fixture.project.name), Some(port));

    let output = fixture.procly.procly_in(&fixture.main, &["up", "--json", "db"]);

    assert_eq!(output.status.code(), Some(5), "stderr: {}", stderr(&output));
    assert!(
        stderr(&output).contains("started outside Procly"),
        "{}",
        stderr(&output)
    );
    let document = common::json_document(&output);
    assert_eq!(document["schema_version"], common::json_schema_version(&fixture.procly));
    assert_eq!(document["result"], serde_json::json!("conflict"));
    assert_eq!(document["service"], serde_json::json!("db"));
    assert_eq!(document["needed_by"], serde_json::json!(null));
    assert_eq!(
        document["conflicts"],
        serde_json::json!([{
            "held": { "kind": "port", "port": port },
            "occupant": { "kind": "container", "name": foreign.name, "project": null, "service": null },
            "stop": format!("docker stop {}", foreign.name),
        }])
    );
    assert!(foreign.is_running(), "Procly stopped a container it does not own");
}

#[test]
fn docker_a_container_holding_the_container_name_without_matching_labels_is_a_conflict() {
    if !require_docker() {
        return;
    }
    let fixture = fixture("");
    let foreign = Foreign::run(&format!("{}-db-1", fixture.project.name), None);

    let output = fixture.procly.procly_in(&fixture.main, &["up", "db"]);

    assert_eq!(output.status.code(), Some(5), "stderr: {}", stderr(&output));
    let error = stderr(&output);
    assert!(
        error.contains(&format!(
            "container name {} is held by a container compose did not create for this service, stop and remove it \
             with `docker rm --force {}`",
            foreign.name, foreign.name
        )),
        "{error}"
    );
    assert!(foreign.is_running(), "Procly stopped a container it does not own");

    run_stop_command(&error);
    assert_success(&fixture.procly.procly_in(&fixture.main, &["up", "db"]));
}

#[test]
fn docker_the_service_running_outside_procly_on_another_port_than_its_lease_is_a_conflict_left_running() {
    if !require_docker() {
        return;
    }
    let fixture = fixture("");
    let elsewhere = free_port();
    compose_up_outside_procly(&fixture, elsewhere);
    let container = fixture.project.container();

    let output = fixture.procly.procly_in(&fixture.main, &["up", "db"]);

    assert_eq!(output.status.code(), Some(5), "stderr: {}", stderr(&output));
    let error = stderr(&output);
    let project = &fixture.project.name;
    assert!(
        error.contains(&format!(
            "compose service db already runs outside Procly in container {project}-db-1, publishing port {elsewhere} \
             instead of its Lease, port {}, stop it with `docker compose --project-name {project} stop db`",
            fixture.port
        )),
        "{error}"
    );
    // Neither stopped nor recreated with the Lease.
    assert!(fixture.project.is_running());
    assert_eq!(fixture.project.container(), container);
    assert!(listens(elsewhere));

    run_stop_command(&error);
    let output = fixture.procly.procly_in(&fixture.main, &["up", "db"]);
    assert_success(&output);
    assert!(stdout(&output).starts_with("db started "), "{}", stdout(&output));
}

#[test]
fn docker_a_container_started_outside_procly_is_a_foreign_process_in_status_until_it_is_adopted() {
    if !require_docker() {
        return;
    }
    let fixture = fixture("");
    let port = fixture.port;
    compose_up_outside_procly(&fixture, port);
    let project = &fixture.project.name;

    let status = stdout(&fixture.procly.procly_in(&fixture.main, &["status"]));

    assert!(
        status.contains(&format!(
            "    foreign container {project}-db-1 (compose project {project}, service db) port {port}, started \
             outside Procly\n"
        )),
        "{status}"
    );
    assert_success(&fixture.procly.procly_in(&fixture.main, &["up", "db"]));
    let status = stdout(&fixture.procly.procly_in(&fixture.main, &["status"]));
    assert!(!status.contains("foreign container"), "{status}");
}

#[test]
fn docker_eject_stops_the_container_at_once_and_keeps_it_with_its_volume() {
    if !require_docker() {
        return;
    }
    let fixture = fixture("grace_period = \"1h\"\n");
    assert_success(&fixture.procly.procly_in(&fixture.main, &["up", "db"]));
    assert_success(&fixture.procly.procly_in(&fixture.other, &["up", "db"]));
    let container = fixture.project.container();
    assert!(fixture.project.is_running());

    let output = fixture.procly.procly_in(&fixture.other, &["eject"]);

    // Without waiting for the Grace period, since the Repository is going back to how it was.
    assert_success(&output);
    assert!(
        stdout(&output).contains("stopped db (Compose service, its volumes are kept)"),
        "{}",
        stdout(&output)
    );
    assert!(!fixture.project.is_running());
    assert!(!listens(fixture.port));
    // Stopped, never removed, and the data stays.
    assert_eq!(fixture.project.container(), container);
    assert!(fixture.project.volume_exists());
}

#[test]
fn an_unreadable_container_document_fails_up_down_status_and_eject_with_125_naming_it() {
    let fixture = fixture("");
    std::fs::create_dir_all(fixture.procly.state_dir()).unwrap();
    let document = fixture.procly.state_dir().join("containers.json");
    std::fs::write(&document, "{ not json").unwrap();

    for args in [&["up", "db"][..], &["down"], &["status"], &["eject"]] {
        let output = fixture.procly.procly_in(&fixture.main, args);

        assert_eq!(output.status.code(), Some(125), "{args:?}");
        assert!(
            stderr(&output).contains(&format!("cannot parse {}, fix or remove it", document.display())),
            "{args:?}: {}",
            stderr(&output)
        );
    }
}

#[test]
fn docker_after_a_daemon_crash_the_container_is_still_counted_for_both_worktrees_until_its_grace_period() {
    if !require_docker() {
        return;
    }
    let fixture = fixture("grace_period = \"1m\"\n");
    assert_success(&fixture.procly.procly_in(&fixture.main, &["up", "db"]));
    assert_success(&fixture.procly.procly_in(&fixture.other, &["up", "db"]));
    let container = fixture.project.container();

    fixture.procly.kill_daemon();

    let port = fixture.port;
    let output = fixture.procly.procly_in(&fixture.other, &["up", "db"]);
    assert_success(&output);
    assert_eq!(
        stdout(&output),
        format!("db reused http://localhost:{port} port {port}\n")
    );
    assert_eq!(fixture.project.container(), container);
    let first = fixture.procly.procly_in(&fixture.main, &["down", "db"]);
    assert_success(&first);
    assert_eq!(stdout(&first), "db released, 1 other Worktree still needs it\n");

    let last = fixture.procly.procly_in(&fixture.other, &["down", "db"]);
    assert_success(&last);
    assert_eq!(
        stdout(&last),
        "db released, it stops after 1m unless a Worktree needs it again\n"
    );
    fixture.procly.advance_clock(Duration::from_secs(61));
    eventually("the container to stop after the Grace period", || {
        !fixture.project.is_running()
    });
    // Stopped, never removed, and the data stays.
    assert_eq!(fixture.project.container(), container);
    assert!(fixture.project.volume_exists());
}

#[test]
fn docker_a_grace_period_running_when_the_daemon_was_killed_resumes_from_its_deadline() {
    if !require_docker() {
        return;
    }
    let fixture = fixture("grace_period = \"1m\"\n");
    assert_success(&fixture.procly.procly_in(&fixture.main, &["up", "db"]));
    assert_success(&fixture.procly.procly_in(&fixture.main, &["down", "db"]));
    fixture.procly.advance_clock(Duration::from_secs(30));

    fixture.procly.kill_daemon();
    assert_success(&fixture.procly.procly_in(&fixture.main, &["status"]));

    fixture.procly.advance_clock(Duration::from_secs(29));
    std::thread::sleep(Duration::from_millis(500));
    assert!(fixture.project.is_running(), "stopped before the recorded deadline");
    // A Grace period started again by the new Daemon would still have half a minute to run.
    fixture.procly.advance_clock(Duration::from_secs(2));
    eventually("the container to stop at its recorded deadline", || {
        !fixture.project.is_running()
    });
}

#[test]
fn docker_a_grace_period_that_ended_while_no_daemon_ran_stops_the_container_at_reconciliation() {
    if !require_docker() {
        return;
    }
    let fixture = fixture("grace_period = \"1m\"\n");
    assert_success(&fixture.procly.procly_in(&fixture.main, &["up", "db"]));
    let container = fixture.project.container();
    assert_success(&fixture.procly.procly_in(&fixture.main, &["down", "db"]));
    fixture.procly.kill_daemon();
    fixture.procly.advance_clock(Duration::from_mins(2));

    assert_success(&fixture.procly.procly_in(&fixture.main, &["status"]));

    // Before `status` answered.
    assert!(!fixture.project.is_running());
    assert_eq!(fixture.project.container(), container);
    assert!(fixture.project.volume_exists());
}

#[test]
fn docker_a_clean_daemon_exit_leaves_the_container_running_and_counted() {
    if !require_docker() {
        return;
    }
    let fixture = fixture("grace_period = \"1m\"\n");
    assert_success(&fixture.procly.procly_in(&fixture.main, &["up", "db"]));
    assert_success(&fixture.procly.procly_in(&fixture.other, &["up", "db"]));
    let container = fixture.project.container();

    fixture.procly.stop_daemon();

    assert!(fixture.project.is_running());
    let output = fixture.procly.procly_in(&fixture.main, &["down", "db"]);
    assert_success(&output);
    assert_eq!(stdout(&output), "db released, 1 other Worktree still needs it\n");
    assert!(stdout(&fixture.procly.procly_in(&fixture.other, &["up", "db"])).starts_with("db reused "));
    assert_eq!(fixture.project.container(), container);
}

#[test]
fn docker_a_worktree_removed_without_down_stops_counting() {
    if !require_docker() {
        return;
    }
    let fixture = fixture("grace_period = \"1m\"\n");
    assert_success(&fixture.procly.procly_in(&fixture.main, &["up", "db"]));
    assert_success(&fixture.procly.procly_in(&fixture.other, &["up", "db"]));
    git(
        &fixture.main,
        &["worktree", "remove", "--force", &fixture.other.to_string_lossy()],
    );

    let output = fixture.procly.procly_in(&fixture.main, &["down", "db"]);

    assert_success(&output);
    assert_eq!(
        stdout(&output),
        "db released, it stops after 1m unless a Worktree needs it again\n"
    );
    fixture.procly.advance_clock(Duration::from_secs(61));
    eventually("the container to stop after the Grace period", || {
        !fixture.project.is_running()
    });
}

#[test]
fn docker_a_worktree_removed_while_no_daemon_ran_starts_the_grace_period_at_reconciliation() {
    if !require_docker() {
        return;
    }
    let fixture = fixture("grace_period = \"1m\"\n");
    assert_success(&fixture.procly.procly_in(&fixture.other, &["up", "db"]));
    fixture.procly.kill_daemon();
    git(
        &fixture.main,
        &["worktree", "remove", "--force", &fixture.other.to_string_lossy()],
    );

    assert_success(&fixture.procly.procly_in(&fixture.main, &["status"]));

    std::thread::sleep(Duration::from_millis(500));
    assert!(fixture.project.is_running(), "stopped without its Grace period");
    fixture.procly.advance_clock(Duration::from_secs(61));
    eventually("the container to stop after the Grace period", || {
        !fixture.project.is_running()
    });
}

#[test]
fn docker_status_and_eject_see_the_container_of_a_previous_daemon() {
    if !require_docker() {
        return;
    }
    let fixture = fixture("grace_period = \"1h\"\n");
    assert_success(&fixture.procly.procly_in(&fixture.main, &["up", "db"]));
    let container = fixture.project.container();
    fixture.procly.kill_daemon();

    let status = stdout(&fixture.procly.procly_in(&fixture.main, &["status"]));
    assert!(!status.contains("foreign container"), "{status}");

    let output = fixture.procly.procly_in(&fixture.main, &["eject"]);
    assert_success(&output);
    assert!(
        stdout(&output).contains("stopped db (Compose service, its volumes are kept)"),
        "{}",
        stdout(&output)
    );
    assert!(!fixture.project.is_running());
    assert_eq!(fixture.project.container(), container);
    assert!(fixture.project.volume_exists());
}

#[test]
fn docker_a_recorded_container_that_no_longer_runs_is_forgotten_and_never_stopped() {
    if !require_docker() {
        return;
    }
    let fixture = fixture("grace_period = \"1m\"\n");
    assert_success(&fixture.procly.procly_in(&fixture.main, &["up", "db"]));
    fixture.procly.kill_daemon();
    fixture.project.docker(&["stop", "db"]);

    assert_success(&fixture.procly.procly_in(&fixture.main, &["status"]));

    // Started again outside Procly, after the new Daemon forgot it.
    fixture.project.docker(&["start", "db"]);
    assert_success(&fixture.procly.procly_in(&fixture.main, &["down", "db"]));
    fixture.procly.advance_clock(Duration::from_mins(2));
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        fixture.project.is_running(),
        "Procly stopped a container it no longer owns"
    );
    assert!(
        fixture
            .procly
            .daemon_log()
            .contains("no longer runs with the labels of its compose service"),
        "{}",
        fixture.procly.daemon_log()
    );
}
