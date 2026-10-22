//! Compose services in Isolated mode: one container per Worktree, under a compose project name of its own. The tests
//! marked Docker drive a real engine and skip themselves when `docker compose` v2 is unavailable, they start containers
//! of projects named after the test and remove them at the end.

mod common;

use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

use common::{
    Procly, assert_success, free_port, git, git_repository, git_worktree_add, json_succeeded, require_docker, stderr,
    stdout,
};

/// A compose file running a TCP listener in `alpine` whose port comes from `DB_PORT`, with its data in a named volume.
fn compose_file(project: &str, extra: &str) -> String {
    format!(
        "name: {project}\nservices:\n  db:\n    image: alpine\n    init: true\n    command: [\"nc\", \"-lk\", \"-p\", \"80\", \"-e\", \"/bin/cat\"]\n    \
         ports:\n      - \"${{DB_PORT:-1}}:80\"\n    volumes:\n      - data:/data\n{extra}volumes:\n  data:\n"
    )
}

/// The `procly.toml` of a Repository whose service `db` is the compose service `db`, one container per Worktree.
fn config(port: u16) -> String {
    format!(
        "[services.db]\ncompose = \"db\"\nisolation = \"isolated\"\nports = {{ db = {port} }}\n\
         env = {{ DB_PORT = \"{{services.db.ports.db}}\" }}\n"
    )
}

/// Removes the containers, network and volumes of one compose project when the test ends, passed or failed.
struct Project {
    name: String,
    dir: PathBuf,
}

impl Project {
    fn try_docker(&self, args: &[&str]) -> std::io::Result<std::process::Output> {
        // A project is found by its name alone once its Worktree is removed.
        let dir = if self.dir.exists() {
            self.dir.clone()
        } else {
            std::env::temp_dir()
        };
        Command::new("docker")
            .args(["compose", "-p", &self.name])
            .args(args)
            .current_dir(dir)
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

/// A Repository with a compose file and a `procly.toml`, and a second Worktree, each with a project of its own.
struct Fixture {
    // First, so that the containers are removed while the working directories still exist.
    main_project: Project,
    other_project: Project,
    procly: Procly,
    main: PathBuf,
    other: PathBuf,
    port: u16,
}

/// The fixture, with `extra` added to the compose service `db`.
fn fixture(extra: &str) -> Fixture {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let procly = Procly::with_clock();
    // The Worktree other than the Main worktree draws from the Range, which parallel tests must not share.
    procly.narrow_range();
    let name = format!(
        "pcl-iso-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let main = git_repository(procly.scratch(), "app");
    let port = free_port();
    std::fs::write(main.join("compose.yaml"), compose_file(&name, extra)).expect("write the compose file");
    std::fs::write(main.join("procly.toml"), config(port)).expect("write the config");
    git(&main, &["add", "."]);
    git(&main, &["commit", "--quiet", "--message", "compose"]);
    let other = git_worktree_add(&main, "app-feature");
    Fixture {
        procly,
        // The Main worktree keeps the project name of the compose file, another one appends its Worktree slug.
        main_project: Project {
            name: name.clone(),
            dir: main.clone(),
        },
        other_project: Project {
            name: format!("{name}-app_feature"),
            dir: other.clone(),
        },
        main,
        other,
        port,
    }
}

fn listens(port: u16) -> bool {
    TcpStream::connect(("127.0.0.1", port)).is_ok()
}

/// The port `up` printed in `db started db port 24001`.
fn printed_port(output: &std::process::Output) -> u16 {
    stdout(output)
        .trim()
        .rsplit(' ')
        .next()
        .and_then(|port| port.parse().ok())
        .unwrap_or_else(|| panic!("no port in {:?}", stdout(output)))
}

#[test]
fn each_worktree_gets_leases_of_its_own_and_the_main_worktree_keeps_the_declared_port() {
    let fixture = fixture("");

    let main = stdout(&fixture.procly.procly_in(&fixture.main, &["env", "db"]));
    let other = stdout(&fixture.procly.procly_in(&fixture.other, &["env", "db"]));

    let port = fixture.port;
    assert!(main.contains(&format!("export PORT_DB='{port}'\n")), "{main}");
    assert!(!other.contains(&format!("PORT_DB='{port}'")), "{other}");
    assert!(other.contains("export PORT_DB='"), "{other}");
}

#[test]
fn a_retention_on_a_shared_or_logical_service_fails_up_with_125_before_anything_starts() {
    let fixture = fixture("");
    for (declared, isolation) in [
        ("", "shared"),
        ("driver = \"postgres\"\nresources = { dev = {} }\n", "logical"),
    ] {
        std::fs::write(
            fixture.main.join("procly.toml"),
            format!("[services.db]\ncompose = \"db\"\nretention = \"delete\"\n{declared}"),
        )
        .expect("write the config");

        let output = fixture.procly.procly_in(&fixture.main, &["up", "db"]);

        assert_eq!(output.status.code(), Some(125), "{}", stderr(&output));
        assert!(
            stderr(&output).contains(&format!(
                "service db sets `retention`, which only an Isolated Compose service has, and its isolation is \
                 \"{isolation}\""
            )),
            "{}",
            stderr(&output)
        );
    }
}

#[test]
fn docker_2_worktrees_run_2_containers_side_by_side_each_on_its_own_lease() {
    let Some(_docker) = require_docker() else {
        return;
    };
    let fixture = fixture("");

    let main = fixture.procly.procly_in(&fixture.main, &["up", "db"]);
    let other = fixture.procly.procly_in(&fixture.other, &["up", "db"]);

    assert_success(&main);
    assert_success(&other);
    let port = fixture.port;
    assert_eq!(stdout(&main), format!("db started db port {port}\n"));
    let other_port = printed_port(&other);
    assert_ne!(other_port, port);
    assert_eq!(stdout(&other), format!("db started db port {other_port}\n"));
    // 2 containers, each in a project of its own and with a volume of its own.
    let (main_container, other_container) = (fixture.main_project.container(), fixture.other_project.container());
    assert!(!main_container.is_empty() && !other_container.is_empty());
    assert_ne!(main_container, other_container);
    assert!(fixture.main_project.is_running() && fixture.other_project.is_running());
    assert!(listens(port) && listens(other_port));
    assert!(fixture.main_project.volume_exists() && fixture.other_project.volume_exists());
}

#[test]
fn docker_up_again_in_the_same_worktree_reuses_its_own_container() {
    let Some(_docker) = require_docker() else {
        return;
    };
    let fixture = fixture("");
    let first = fixture.procly.procly_in(&fixture.other, &["up", "db"]);
    assert_success(&first);
    let container = fixture.other_project.container();

    let again = fixture.procly.procly_in(&fixture.other, &["up", "--json", "db"]);

    let document = json_succeeded(&again);
    let port = printed_port(&first);
    assert_eq!(document["services"][0]["outcome"], "reused", "{document}");
    assert_eq!(document["services"][0]["ports"], serde_json::json!({ "db": port }));
    assert_eq!(fixture.other_project.container(), container);
    // Reusing the container of one Worktree never starts the one of another.
    assert_eq!(fixture.main_project.container(), "");
}

#[test]
fn docker_a_compose_file_that_fixes_the_container_name_is_refused_with_the_offending_line() {
    let Some(_docker) = require_docker() else {
        return;
    };
    let fixture = fixture("    container_name: pcl-iso-fixed-db\n");

    let output = fixture.procly.procly_in(&fixture.other, &["up", "db"]);

    assert_eq!(output.status.code(), Some(125), "{}", stderr(&output));
    let message = stderr(&output);
    let compose = fixture.other.join("compose.yaml");
    assert!(
        message.contains(&format!("{}:11: container_name: pcl-iso-fixed-db", compose.display())),
        "{message}"
    );
    assert!(message.contains("isolation = \"isolated\""), "{message}");
    assert!(fixture.other_project.container().is_empty(), "a container was created");
}

#[test]
fn docker_a_compose_file_that_fixes_a_network_name_is_refused_with_the_offending_line() {
    let Some(_docker) = require_docker() else {
        return;
    };
    let fixture = fixture("    networks:\n      - back\nnetworks:\n  back:\n    name: pcl-iso-fixed-net\n");

    let output = fixture.procly.procly_in(&fixture.main, &["up", "db"]);

    assert_eq!(output.status.code(), Some(125), "{}", stderr(&output));
    let message = stderr(&output);
    let compose = fixture.main.join("compose.yaml");
    assert!(
        message.contains(&format!("{}:15: name: pcl-iso-fixed-net", compose.display())),
        "{message}"
    );
    assert!(fixture.main_project.container().is_empty(), "a container was created");
}

#[test]
fn docker_down_stops_the_container_of_the_worktree_at_once_and_keeps_it_with_its_volume() {
    let Some(_docker) = require_docker() else {
        return;
    };
    let fixture = fixture("");
    assert_success(&fixture.procly.procly_in(&fixture.main, &["up", "db"]));
    let other_port = printed_port(&fixture.procly.procly_in(&fixture.other, &["up", "db"]));
    let container = fixture.other_project.container();

    let output = fixture.procly.procly_in(&fixture.other, &["down", "db"]);

    assert_success(&output);
    assert_eq!(stdout(&output), "db stopped\n");
    // Without a Grace period, nobody else needs it.
    assert!(!fixture.other_project.is_running());
    assert!(!listens(other_port));
    assert_eq!(fixture.other_project.container(), container);
    assert!(fixture.other_project.volume_exists());
    // The container of the other Worktree is its own business.
    assert!(fixture.main_project.is_running());
    let again = fixture.procly.procly_in(&fixture.other, &["down", "db"]);
    assert_eq!(stdout(&again), "db not running\n");
}

#[test]
fn docker_a_worktree_release_stops_the_container_of_the_worktree_at_once_and_releases_its_lease() {
    let Some(_docker) = require_docker() else {
        return;
    };
    let fixture = fixture("");
    assert_success(&fixture.procly.procly_in(&fixture.main, &["up", "db"]));
    let other_port = printed_port(&fixture.procly.procly_in(&fixture.other, &["up", "db"]));
    let container = fixture.other_project.container();

    let output = fixture.procly.procly_in(&fixture.other, &["worktree", "release"]);

    assert_success(&output);
    assert_eq!(
        stdout(&output),
        format!("db stopped\nlease db.db port {other_port} released\n")
    );
    assert!(!fixture.other_project.is_running());
    assert!(!listens(other_port));
    assert_eq!(fixture.other_project.container(), container);
    assert!(fixture.other_project.volume_exists());
    // The container of the other Worktree is its own business.
    assert!(fixture.main_project.is_running());
    let again = fixture.procly.procly_in(&fixture.other, &["worktree", "release"]);
    assert_eq!(stdout(&again), "nothing to release\n");
}

#[test]
fn docker_status_shows_the_containers_of_the_worktree_and_its_json_lists_them() {
    let Some(_docker) = require_docker() else {
        return;
    };
    let fixture = fixture("");
    assert_success(&fixture.procly.procly_in(&fixture.main, &["up", "db"]));
    let other_port = printed_port(&fixture.procly.procly_in(&fixture.other, &["up", "db"]));
    let project = &fixture.other_project.name;

    let text = stdout(&fixture.procly.procly_in(&fixture.other, &["status"]));
    let document = json_succeeded(&fixture.procly.procly_in(&fixture.other, &["status", "--json"]));

    assert!(
        text.contains(&format!(
            "    container of service db (compose project {project}, service db) port {other_port}, Isolated\n"
        )),
        "{text}"
    );
    // Only the container of the current Worktree, and never as a Foreign process.
    let main_project = &fixture.main_project.name;
    assert!(!text.contains(&format!("compose project {main_project}, ")), "{text}");
    assert!(!text.contains("foreign"), "{text}");
    let worktree = &document["repositories"][0]["worktrees"][0];
    assert_eq!(
        worktree["containers"],
        serde_json::json!([{ "service": "db", "project": project, "compose": "db", "port": other_port }]),
        "{document}"
    );
}

#[test]
fn docker_eject_stops_the_containers_of_every_worktree_and_keeps_them_with_their_volumes() {
    let Some(_docker) = require_docker() else {
        return;
    };
    let fixture = fixture("");
    assert_success(&fixture.procly.procly_in(&fixture.main, &["up", "db"]));
    assert_success(&fixture.procly.procly_in(&fixture.other, &["up", "db"]));
    let containers = (fixture.main_project.container(), fixture.other_project.container());

    let output = fixture.procly.procly_in(&fixture.main, &["eject"]);

    assert_success(&output);
    let text = stdout(&output);
    for worktree in [&fixture.main, &fixture.other] {
        assert!(
            text.contains(&format!(
                "stopped db of {} (Isolated Compose service, its container and volumes are kept)",
                worktree.display()
            )),
            "{text}"
        );
    }
    assert!(!fixture.main_project.is_running() && !fixture.other_project.is_running());
    assert_eq!(
        (fixture.main_project.container(), fixture.other_project.container()),
        containers
    );
    assert!(fixture.main_project.volume_exists() && fixture.other_project.volume_exists());
}

#[test]
fn docker_logs_reads_the_container_of_the_worktree() {
    let Some(_docker) = require_docker() else {
        return;
    };
    let fixture = fixture("");
    let talking = |marker: &str| {
        compose_file(&fixture.main_project.name, "").replace(
            "command: [\"nc\", \"-lk\", \"-p\", \"80\", \"-e\", \"/bin/cat\"]",
            &format!("command: [\"sh\", \"-c\", \"echo {marker}; exec nc -lk -p 80 -e /bin/cat\"]"),
        )
    };
    write(&fixture.main, &talking("procly-main-marker"));
    write(&fixture.other, &talking("procly-other-marker"));
    assert_success(&fixture.procly.procly_in(&fixture.main, &["up", "db"]));
    assert_success(&fixture.procly.procly_in(&fixture.other, &["up", "db"]));

    let output = fixture.procly.procly_in(&fixture.other, &["logs", "db"]);

    assert_success(&output);
    let logs = stdout(&output);
    assert!(logs.contains("procly-other-marker"), "{logs}");
    assert!(!logs.contains("procly-main-marker"), "{logs}");
}

#[test]
fn docker_the_container_of_a_worktree_removed_without_down_stops_on_the_next_down() {
    let Some(_docker) = require_docker() else {
        return;
    };
    let fixture = fixture("");
    assert_success(&fixture.procly.procly_in(&fixture.main, &["up", "db"]));
    assert_success(&fixture.procly.procly_in(&fixture.other, &["up", "db"]));
    let container = fixture.other_project.container();
    git(
        &fixture.main,
        &["worktree", "remove", "--force", &fixture.other.to_string_lossy()],
    );

    let output = fixture.procly.procly_in(&fixture.main, &["down", "db"]);

    assert_success(&output);
    // The Main worktree tells of its own container only.
    assert_eq!(stdout(&output), "db stopped\n");
    assert!(!fixture.other_project.is_running());
    assert_eq!(fixture.other_project.container(), container);
    assert!(fixture.other_project.volume_exists());
}

#[test]
fn docker_after_a_daemon_crash_the_container_of_a_worktree_is_still_its_own() {
    let Some(_docker) = require_docker() else {
        return;
    };
    let fixture = fixture("");
    let first = fixture.procly.procly_in(&fixture.other, &["up", "db"]);
    assert_success(&first);
    let container = fixture.other_project.container();

    fixture.procly.kill_daemon();

    let again = fixture.procly.procly_in(&fixture.other, &["up", "db"]);
    assert_success(&again);
    let port = printed_port(&first);
    assert_eq!(stdout(&again), format!("db reused db port {port}\n"));
    assert_eq!(fixture.other_project.container(), container);
    let status = stdout(&fixture.procly.procly_in(&fixture.other, &["status"]));
    assert!(status.contains("container of service db (compose project "), "{status}");
    // The Main worktree never had a container, and the one of the other Worktree is not its own.
    let down = fixture.procly.procly_in(&fixture.main, &["down", "db"]);
    assert_eq!(stdout(&down), "db not running\n");
    assert!(fixture.other_project.is_running());
    assert_success(&fixture.procly.procly_in(&fixture.other, &["down", "db"]));
    assert!(!fixture.other_project.is_running());
}

/// Replaces the compose file of the Worktree `worktree`, which reads its own.
fn write(worktree: &Path, compose: &str) {
    std::fs::write(worktree.join("compose.yaml"), compose).expect("write the compose file");
}

/// Brings up `db` in the Main worktree and in the other one, stops the one of the other Worktree, and removes that
/// Worktree as a worktree tool does, so that its container and its volume are left behind.
fn leave_behind_the_other_container(fixture: &Fixture) {
    assert_success(&fixture.procly.procly_in(&fixture.main, &["up", "db"]));
    assert_success(&fixture.procly.procly_in(&fixture.other, &["up", "db"]));
    assert_success(&fixture.procly.procly_in(&fixture.other, &["down", "db"]));
    git(
        &fixture.main,
        &["worktree", "remove", "--force", &fixture.other.to_string_lossy()],
    );
}

/// The lines of `gc` about Isolated containers and volumes, the Leases aside.
fn isolated_lines(text: &str) -> Vec<String> {
    text.lines()
        .filter(|line| line.contains("isolated container ") || line.contains("isolated volume "))
        .map(str::to_owned)
        .collect()
}

#[test]
fn docker_gc_lists_the_container_and_volume_of_a_removed_worktree_and_gc_apply_removes_them() {
    let Some(_docker) = require_docker() else {
        return;
    };
    let fixture = fixture("");
    leave_behind_the_other_container(&fixture);
    let project = &fixture.other_project.name;
    let worktree = fixture.other.display();

    let listed = fixture.procly.procly(&["gc"]);

    assert_success(&listed);
    assert_eq!(
        isolated_lines(&stdout(&listed)),
        [
            format!("isolated container db (compose project {project}, service db) of {worktree}"),
            format!("isolated volume {project}_data (compose project {project}) of {worktree}"),
        ],
        "{}",
        stdout(&listed)
    );
    assert!(
        !fixture.other_project.container().is_empty(),
        "gc without --apply removed the container"
    );
    assert!(fixture.other_project.volume_exists());
    let document = json_succeeded(&fixture.procly.procly(&["gc", "--json"]));
    let orphans = document["orphans"].as_array().expect("a list of Orphans");
    assert!(
        orphans.contains(&serde_json::json!({
            "kind": "isolated_container", "service": "db", "project": project, "compose": "db",
            "repository": orphans[0]["repository"], "worktree": fixture.other, "deleted": false, "error": null,
        })),
        "{document}"
    );
    assert!(
        orphans.contains(&serde_json::json!({
            "kind": "isolated_volume", "project": project, "volume": format!("{project}_data"),
            "repository": orphans[0]["repository"], "worktree": fixture.other, "deleted": false, "error": null,
        })),
        "{document}"
    );

    // A container of another directory under the same compose project and service is not the one Procly started.
    let impostor = Holder(format!("{project}-impostor"));
    let created = Command::new("docker")
        .args(["create", "--name", &impostor.0])
        .args(["--label", &format!("com.docker.compose.project={project}")])
        .args(["--label", "com.docker.compose.service=db"])
        .args(["--label", "com.docker.compose.project.working_dir=/elsewhere"])
        .args(["alpine", "true"])
        .output()
        .expect("docker runs");
    assert_success(&created);

    let applied = fixture.procly.procly(&["gc", "--apply"]);

    assert_success(&applied);
    assert_eq!(
        isolated_lines(&stdout(&applied)),
        [
            format!("deleted isolated container db (compose project {project}, service db) of {worktree}"),
            format!("deleted isolated volume {project}_data (compose project {project}) of {worktree}"),
        ],
        "{}",
        stdout(&applied)
    );
    assert!(fixture.other_project.container().is_empty(), "the container is left");
    assert!(
        !impostor_id(&impostor).is_empty(),
        "the container of another directory was removed"
    );
    assert!(!fixture.other_project.volume_exists(), "the volume is left");
    // The container of the Main worktree is not an Orphan.
    assert!(fixture.main_project.is_running() && fixture.main_project.volume_exists());
    assert_eq!(stdout(&fixture.procly.procly(&["gc"])), "no Orphans\n");
}

#[test]
fn docker_gc_never_removes_the_container_of_a_worktree_git_still_lists_or_of_a_pinned_branch() {
    let Some(_docker) = require_docker() else {
        return;
    };
    let fixture = fixture("");
    // The Worktree is on the branch `app-feature`, which the Worktree pins.
    let pinned = format!("{}[pinned.app-feature]\n", config(fixture.port));
    std::fs::write(fixture.other.join("procly.toml"), pinned).expect("write the config");
    assert_success(&fixture.procly.procly_in(&fixture.other, &["up", "db"]));
    assert_success(&fixture.procly.procly_in(&fixture.other, &["down", "db"]));
    // Its directory is gone, but git still lists it, as prunable.
    std::fs::remove_dir_all(&fixture.other).expect("remove the Worktree directory");

    let listed = stdout(&fixture.procly.procly(&["gc"]));

    assert_eq!(isolated_lines(&listed), Vec::<String>::new(), "{listed}");
    git(&fixture.main, &["worktree", "prune"]);

    let applied = fixture.procly.procly(&["gc", "--apply"]);

    assert_success(&applied);
    assert_eq!(
        isolated_lines(&stdout(&applied)),
        Vec::<String>::new(),
        "{}",
        stdout(&applied)
    );
    assert!(
        !fixture.other_project.container().is_empty(),
        "the container of a Pinned branch was removed"
    );
    assert!(fixture.other_project.volume_exists());
}

#[test]
fn docker_a_volume_gc_apply_cannot_remove_is_reported_and_removed_by_the_next_gc_apply() {
    let Some(_docker) = require_docker() else {
        return;
    };
    let fixture = fixture("");
    leave_behind_the_other_container(&fixture);
    let project = &fixture.other_project.name;
    let volume = format!("{project}_data");
    // A container outside Procly holds the volume, so that the engine refuses to remove it.
    let holder = Holder(format!("{project}-holder"));
    let held = Command::new("docker")
        .args(["run", "--detach", "--name", &holder.0, "--volume"])
        .arg(format!("{volume}:/data"))
        .args(["alpine", "sleep", "300"])
        .output()
        .expect("docker runs");
    assert_success(&held);
    let worktree = fixture.other.display();

    let failed = fixture.procly.procly(&["gc", "--apply"]);

    assert_eq!(failed.status.code(), Some(6), "{}", stderr(&failed));
    assert_eq!(
        isolated_lines(&stdout(&failed)),
        [format!(
            "deleted isolated container db (compose project {project}, service db) of {worktree}"
        )],
        "{}",
        stdout(&failed)
    );
    let message = stderr(&failed);
    assert!(
        message.contains(&format!(
            "procly: cannot delete isolated volume {volume} (compose project {project}) of {worktree}: "
        )),
        "{message}"
    );
    assert!(fixture.other_project.volume_exists());
    // The volume stays an Orphan, though its container is gone.
    let listed = stdout(&fixture.procly.procly(&["gc"]));
    assert_eq!(
        isolated_lines(&listed),
        [format!(
            "isolated volume {volume} (compose project {project}) of {worktree}"
        )],
        "{listed}"
    );
    drop(holder);

    let applied = fixture.procly.procly(&["gc", "--apply"]);

    assert_success(&applied);
    assert_eq!(
        isolated_lines(&stdout(&applied)),
        [format!(
            "deleted isolated volume {volume} (compose project {project}) of {worktree}"
        )],
        "{}",
        stdout(&applied)
    );
    assert!(!fixture.other_project.volume_exists());
}

#[test]
fn docker_gc_never_removes_the_container_compose_reused_for_a_worktree_of_the_same_compose_project() {
    let Some(_docker) = require_docker() else {
        return;
    };
    let fixture = fixture("");
    let other_port = printed_port(&fixture.procly.procly_in(&fixture.other, &["up", "db"]));
    assert_success(&fixture.procly.procly_in(&fixture.other, &["down", "db"]));
    git(
        &fixture.main,
        &["worktree", "remove", "--force", &fixture.other.to_string_lossy()],
    );
    let container = fixture.other_project.container();
    // A Worktree of the same directory name has the same compose project. With the same configuration, compose
    // starts the stopped container again for it, labelled with the working directory of the removed Worktree.
    let again = fixture.procly.scratch().join("again").join("app-feature");
    git(
        &fixture.main,
        &["worktree", "add", "--quiet", "-b", "again", &again.to_string_lossy()],
    );
    let again = again.canonicalize().expect("canonicalize the worktree");
    write(
        &again,
        &compose_file(&fixture.main_project.name, "").replace("${DB_PORT:-1}", &other_port.to_string()),
    );
    std::fs::write(
        again.join("procly.toml"),
        "[services.db]\ncompose = \"db\"\nisolation = \"isolated\"\n",
    )
    .expect("write the config");
    assert_success(&fixture.procly.procly_in(&again, &["up", "db"]));
    assert_eq!(
        fixture.other_project.container(),
        container,
        "compose created another container"
    );

    let running = fixture.procly.procly(&["gc", "--apply"]);

    assert_success(&running);
    assert_eq!(
        isolated_lines(&stdout(&running)),
        Vec::<String>::new(),
        "{}",
        stdout(&running)
    );
    assert!(
        fixture.other_project.is_running(),
        "gc stopped the container of a Worktree"
    );
    assert_success(&fixture.procly.procly_in(&again, &["down", "db"]));

    let stopped = fixture.procly.procly(&["gc", "--apply"]);

    assert_success(&stopped);
    assert_eq!(
        isolated_lines(&stdout(&stopped)),
        Vec::<String>::new(),
        "{}",
        stdout(&stopped)
    );
    assert_eq!(
        fixture.other_project.container(),
        container,
        "gc removed the container of a Worktree"
    );
    assert!(
        fixture.other_project.volume_exists(),
        "gc removed the volume of a Worktree"
    );
}

/// The id of the container `holder`, empty when it no longer exists.
fn impostor_id(holder: &Holder) -> String {
    let output = Command::new("docker")
        .args(["inspect", "--format", "{{.Id}}", &holder.0])
        .output()
        .expect("docker runs");
    stdout(&output).trim().to_owned()
}

/// A container started outside Procly, removed when the test ends.
struct Holder(String);

impl Drop for Holder {
    fn drop(&mut self) {
        let _ = Command::new("docker").args(["rm", "--force", &self.0]).output();
    }
}

/// Gives the other Worktree of `fixture` the Retention policy `retention`, then leaves its container behind.
fn leave_behind_with_retention(fixture: &Fixture, retention: &str) {
    std::fs::write(
        fixture.other.join("procly.toml"),
        format!("{}retention = \"{retention}\"\n", config(fixture.port)),
    )
    .expect("write the config");
    leave_behind_the_other_container(fixture);
}

#[test]
fn docker_a_retention_delete_removes_the_container_and_volume_of_a_removed_worktree_on_the_next_release() {
    let Some(_docker) = require_docker() else {
        return;
    };
    let fixture = fixture("");
    leave_behind_with_retention(&fixture, "delete");
    assert!(
        !fixture.other_project.container().is_empty(),
        "the container is deleted before the release"
    );
    let project = &fixture.other_project.name;
    let worktree = fixture.other.display();

    let released = fixture.procly.procly_in(&fixture.main, &["worktree", "release"]);

    assert_success(&released);
    assert!(fixture.other_project.container().is_empty(), "the container is left");
    assert!(!fixture.other_project.volume_exists(), "the volume is left");
    let log = fixture.procly.daemon_log();
    assert!(
        log.contains(&format!(
            "retention policy delete of service db deleted isolated container db (compose project {project}, service \
             db) of {worktree}"
        )),
        "{log}"
    );
    assert!(
        log.contains(&format!(
            "retention policy delete of service db deleted isolated volume {project}_data (compose project {project}) \
             of {worktree}"
        )),
        "{log}"
    );
    // Deleted, they are no longer Orphans, and the container of the Main worktree is its own business.
    assert_eq!(
        isolated_lines(&stdout(&fixture.procly.procly(&["gc"]))),
        Vec::<String>::new()
    );
    assert!(!fixture.main_project.container().is_empty() && fixture.main_project.volume_exists());
}

#[test]
fn docker_the_retention_declared_when_the_container_last_came_up_is_the_one_applied() {
    let Some(_docker) = require_docker() else {
        return;
    };
    let fixture = fixture("");
    assert_success(&fixture.procly.procly_in(&fixture.other, &["up", "db"]));
    std::fs::write(
        fixture.other.join("procly.toml"),
        format!("{}retention = \"delete\"\n", config(fixture.port)),
    )
    .expect("write the config");
    let reused = fixture.procly.procly_in(&fixture.other, &["up", "db"]);
    assert!(stdout(&reused).starts_with("db reused"), "{}", stdout(&reused));
    assert_success(&fixture.procly.procly_in(&fixture.other, &["down", "db"]));
    git(
        &fixture.main,
        &["worktree", "remove", "--force", &fixture.other.to_string_lossy()],
    );

    assert_success(&fixture.procly.procly_in(&fixture.main, &["worktree", "release"]));

    assert!(fixture.other_project.container().is_empty(), "the container is left");
    assert!(!fixture.other_project.volume_exists(), "the volume is left");
}

#[test]
fn docker_a_retention_delete_removes_the_container_of_a_removed_worktree_when_the_daemon_reconciles() {
    let Some(_docker) = require_docker() else {
        return;
    };
    let fixture = fixture("");
    leave_behind_with_retention(&fixture, "delete");
    fixture.procly.stop_daemon();
    assert!(
        !fixture.other_project.container().is_empty(),
        "the container is deleted before the Daemon reconciles"
    );

    assert_success(&fixture.procly.procly_in(&fixture.main, &["status"]));

    common::eventually("the container and volume are deleted", || {
        fixture.other_project.container().is_empty() && !fixture.other_project.volume_exists()
    });
    let log = fixture.procly.daemon_log();
    assert!(
        log.contains(&format!(
            "retention policy delete of service db deleted isolated container db (compose project {}, service db) of \
             {}",
            fixture.other_project.name,
            fixture.other.display()
        )),
        "{log}"
    );
}

#[test]
fn docker_a_retention_keep_never_deletes_the_container_of_a_removed_worktree() {
    let Some(_docker) = require_docker() else {
        return;
    };
    let fixture = fixture("");
    leave_behind_with_retention(&fixture, "keep");

    assert_success(&fixture.procly.procly_in(&fixture.main, &["worktree", "release"]));
    fixture.procly.stop_daemon();
    assert_success(&fixture.procly.procly_in(&fixture.main, &["status"]));
    fixture.procly.advance_clock(std::time::Duration::from_hours(24 * 365));
    assert_success(&fixture.procly.procly_in(&fixture.main, &["worktree", "release"]));

    assert!(
        !fixture.other_project.container().is_empty(),
        "the container was deleted"
    );
    assert!(fixture.other_project.volume_exists(), "the volume was deleted");
    // `gc --apply` is the only way it goes.
    assert_eq!(isolated_lines(&stdout(&fixture.procly.procly(&["gc"]))).len(), 2);
    assert!(!fixture.procly.daemon_log().contains("retention policy"));
}

#[test]
fn docker_a_retention_duration_deletes_that_long_after_the_worktree_was_found_removed_across_a_daemon_restart() {
    use std::time::Duration;

    let Some(_docker) = require_docker() else {
        return;
    };
    let fixture = fixture("");
    leave_behind_with_retention(&fixture, "1h");
    // Long enough for a deletion run early to be seen.
    let settle = || std::thread::sleep(Duration::from_millis(500));
    let kept = |when: &str| {
        assert!(
            !fixture.other_project.container().is_empty(),
            "the container is deleted {when}"
        );
        assert!(fixture.other_project.volume_exists(), "the volume is deleted {when}");
    };
    // Removed for a while before the Daemon finds it, the hour counts from when it does.
    fixture.procly.advance_clock(Duration::from_hours(5));

    assert_success(&fixture.procly.procly_in(&fixture.main, &["worktree", "release"]));
    settle();
    kept("at once");
    let log = fixture.procly.daemon_log();
    assert!(
        log.contains(&format!(
            "the Worktree {} is removed, retention policy 1h of service db deletes the isolated container of service \
             db of {}",
            fixture.other.display(),
            fixture.other.display()
        )),
        "{log}"
    );
    fixture.procly.advance_clock(Duration::from_mins(30));
    settle();
    kept("after 30 minutes");

    // The pending deletion outlives the Daemon, and is not run early by the next one.
    fixture.procly.stop_daemon();
    assert_success(&fixture.procly.procly_in(&fixture.main, &["status"]));
    fixture.procly.advance_clock(Duration::from_mins(29));
    settle();
    kept("after 59 minutes and a restart");

    fixture.procly.advance_clock(Duration::from_mins(1));

    common::eventually("the container and volume are deleted", || {
        fixture.other_project.container().is_empty() && !fixture.other_project.volume_exists()
    });
    let log = fixture.procly.daemon_log();
    assert!(
        log.contains(&format!(
            "retention policy 1h of service db deleted isolated volume {}_data (compose project {}) of {}",
            fixture.other_project.name,
            fixture.other_project.name,
            fixture.other.display()
        )),
        "{log}"
    );
}
