//! `procly doctor`: checks that Procly can work here, tells the command that fixes each problem and changes nothing.
//!
//! Every run has a home directory, a state directory and a configuration directory of its own, and fake `docker`,
//! `launchctl` and `systemctl` first in the `PATH`, so that no test reaches the Session unit, the agent configuration
//! or the Docker engine of the developer.

mod common;

use std::collections::BTreeMap;
use std::io::{BufRead as _, BufReader, Write as _};
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};

use common::{
    Procly, assert_success, eventually, free_port, git, git_repository, git_worktree_add, json_document,
    json_schema_version, json_succeeded, stderr, stdout, test_server,
};
use serde_json::{Value, json};

/// A fake `launchctl` or `systemctl` that remembers whether the unit is loaded in a file next to it, and logs its calls.
const FAKE_MANAGER: &str = r#"#!/bin/sh
dir=$(dirname "$0")
echo "$(basename "$0") $*" >> "$dir/calls"
if [ "$1" = "--user" ]; then shift; fi
case "$1" in
    print) [ -f "$dir/loaded" ] || exit 113; echo "state = running" ;;
    bootstrap) touch "$dir/loaded" ;;
    bootout) [ -f "$dir/loaded" ] || exit 3; rm "$dir/loaded" ;;
    enable) touch "$dir/loaded" ;;
    disable) rm -f "$dir/loaded" ;;
    is-enabled) if [ -f "$dir/loaded" ]; then echo enabled; else echo disabled; exit 1; fi ;;
    is-active) if [ -f "$dir/loaded" ]; then echo active; else echo inactive; exit 3; fi ;;
esac
"#;

/// A fake `docker` whose `compose version` answers with the content of `compose-version` next to it, or fails when
/// that file is missing. Any other command lists nothing.
const FAKE_DOCKER: &str = r#"#!/bin/sh
dir=$(dirname "$0")
if [ "$1" = "compose" ] && [ "$2" = "version" ]; then
    [ -f "$dir/compose-version" ] || { echo "docker: 'compose' is not a docker command." >&2; exit 1; }
    cat "$dir/compose-version"
    exit 0
fi
exit 0
"#;

/// A healthy machine for `doctor`: a Repository, a home directory and fake tools of its own.
struct Setup {
    procly: Procly,
    home: PathBuf,
    bin: PathBuf,
    repo: PathBuf,
}

impl Setup {
    fn new() -> Self {
        let procly = Procly::new();
        let (home, bin) = (procly.scratch().join("home"), procly.scratch().join("bin"));
        std::fs::create_dir_all(&home).expect("create the home directory");
        std::fs::create_dir_all(&bin).expect("create the fake tools directory");
        for (tool, script) in [
            ("launchctl", FAKE_MANAGER),
            ("systemctl", FAKE_MANAGER),
            ("docker", FAKE_DOCKER),
        ] {
            let path = bin.join(tool);
            std::fs::write(&path, script).expect("write a fake tool");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("make it executable");
        }
        std::fs::write(bin.join("compose-version"), "2.29.1\n").expect("write the fake compose version");
        let repo = git_repository(procly.scratch(), "app");
        Self {
            procly,
            home,
            bin,
            repo,
        }
    }

    /// `procly` with `args` in `dir`, with the home directory and the fake tools of this setup.
    fn run(&self, dir: &Path, args: &[&str]) -> Output {
        self.command(dir).args(args).output().expect("the procly binary runs")
    }

    fn command(&self, dir: &Path) -> Command {
        let path = format!("{}:{}", self.bin.display(), std::env::var("PATH").unwrap_or_default());
        let mut command = self.procly.command();
        command
            .current_dir(dir)
            .env("HOME", &self.home)
            .env_remove("XDG_CONFIG_HOME")
            .env("PATH", path);
        command
    }

    /// `procly doctor --json` in the Repository.
    fn doctor(&self) -> Output {
        self.run(&self.repo, &["doctor", "--json"])
    }

    /// Declares `web` in the Repository, listening on its leased port, declared as `port`.
    fn declare_web(&self, port: u16) {
        write(
            &self.repo,
            "procly.toml",
            &format!(
                "[services.web]\nrun = '{} --port {{ports.http}}'\nports = {{ http = {port} }}\n",
                test_server().display()
            ),
        );
    }

    /// Installs the Session unit, which the fake session manager loads, and the Guard in the Repository.
    fn install_unit_and_guard(&self) {
        assert_success(&self.run(&self.repo, &["unit", "install"]));
        assert_success(&self.run(&self.repo, &["agent", "install"]));
    }

    /// Makes every check pass.
    fn healthy() -> Self {
        let setup = Self::new();
        setup.declare_web(free_port());
        setup.install_unit_and_guard();
        setup
    }

    fn unit_calls(&self) -> String {
        std::fs::read_to_string(self.bin.join("calls")).unwrap_or_default()
    }
}

fn write(dir: &Path, name: &str, content: &str) {
    std::fs::write(dir.join(name), content).expect("write the file");
}

/// The check `name` of a `doctor --json` document.
fn check<'a>(document: &'a Value, name: &str) -> &'a Value {
    document["checks"]
        .as_array()
        .and_then(|checks| checks.iter().find(|check| check["name"] == json!(name)))
        .unwrap_or_else(|| panic!("no check {name} in {document}"))
}

fn status<'a>(document: &'a Value, name: &str) -> &'a str {
    check(document, name)["status"].as_str().expect("a status is a string")
}

fn message<'a>(document: &'a Value, name: &str) -> &'a str {
    check(document, name)["message"]
        .as_str()
        .expect("a message is a string")
}

/// The document of a `doctor --json` that found a problem: exit 7, and nothing on stderr.
fn problems(output: &Output) -> Value {
    assert_eq!(output.status.code(), Some(7), "stderr: {}", stderr(output));
    assert_eq!(stderr(output), "", "--json prints nothing on stderr");
    let document = json_document(output);
    assert_eq!(document["result"], json!("problems"));
    document
}

const ALL_CHECKS: [&str; 8] = [
    "docker_compose",
    "daemon",
    "session_unit",
    "state_documents",
    "configuration",
    "guard",
    "leased_ports",
    "orphans",
];

#[test]
fn a_healthy_setup_passes_every_check_and_exits_0() {
    let setup = Setup::healthy();

    let output = setup.doctor();

    let document = json_succeeded(&output);
    assert_eq!(document["result"], json!("healthy"));
    let names: Vec<&str> = document["checks"]
        .as_array()
        .expect("checks is a list")
        .iter()
        .map(|check| check["name"].as_str().expect("a name"))
        .collect();
    assert_eq!(names, ALL_CHECKS);
    for name in ALL_CHECKS {
        assert_eq!(
            status(&document, name),
            "passed",
            "{name}: {}",
            message(&document, name)
        );
        assert_eq!(check(&document, name)["fix"], Value::Null, "{name}");
    }
    assert_eq!(document["schema_version"], json_schema_version(&setup.procly));
}

#[test]
fn the_text_of_a_healthy_setup_lists_one_passed_line_per_check_and_exits_0() {
    let setup = Setup::healthy();

    let output = setup.run(&setup.repo, &["doctor"]);

    assert_success(&output);
    assert_eq!(stderr(&output), "");
    let text = stdout(&output);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), ALL_CHECKS.len(), "{text}");
    for (line, name) in lines.iter().zip(ALL_CHECKS) {
        assert!(line.starts_with(&format!("passed  {name}: ")), "{line}");
    }
}

#[test]
fn a_missing_docker_compose_is_a_problem_and_the_other_checks_still_run() {
    let setup = Setup::healthy();
    std::fs::remove_file(setup.bin.join("compose-version")).expect("remove the fake compose version");

    let output = setup.doctor();

    let document = problems(&output);
    assert_eq!(status(&document, "docker_compose"), "failed");
    assert!(message(&document, "docker_compose").contains("docker compose v2 is unavailable"));
    let fix = &check(&document, "docker_compose")["fix"];
    if cfg!(target_os = "macos") {
        assert_eq!(fix, &json!("brew install --cask docker"));
    } else {
        assert_eq!(fix, &Value::Null);
    }
    assert_eq!(status(&document, "daemon"), "passed");
    assert_eq!(status(&document, "orphans"), "passed");
}

#[test]
fn docker_compose_v1_is_a_problem() {
    let setup = Setup::healthy();
    write(&setup.bin, "compose-version", "1.29.2\n");

    let document = problems(&setup.doctor());

    assert_eq!(status(&document, "docker_compose"), "failed");
    assert!(message(&document, "docker_compose").contains("found docker compose 1.29.2"));
}

/// A Daemon of another version, standing on the socket of `state_dir`: it answers the request of `doctor` with
/// `answer`, under the protocol version the request carried unless `speaks` says another one.
fn fake_daemon(state_dir: &Path, answer: Value, speaks: Option<u64>) -> std::thread::JoinHandle<()> {
    let listener = UnixListener::bind(state_dir.join("procly.sock")).expect("bind the fake Daemon socket");
    std::thread::spawn(move || {
        // `doctor` asks once, a second connection ends the thread with the test.
        let Ok((stream, _)) = listener.accept() else { return };
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line).expect("read the request");
        let request: Value = serde_json::from_str(&line).expect("the request is JSON");
        let mut response = answer;
        response["version"] = speaks.map_or_else(|| request["version"].clone(), |version| json!(version));
        let mut stream = &stream;
        writeln!(stream, "{response}").expect("write the response");
    })
}

#[test]
fn a_daemon_of_another_version_is_a_problem_and_the_fix_restarts_it() {
    let setup = Setup::new();
    let daemon = fake_daemon(
        setup.procly.state_dir(),
        json!({ "response": "doctor", "procly_version": "0.0.1", "checks": [] }),
        None,
    );

    let document = problems(&setup.doctor());

    daemon.join().expect("the fake Daemon ends");
    assert_eq!(status(&document, "daemon"), "failed");
    assert!(
        message(&document, "daemon").contains("the Daemon runs procly 0.0.1"),
        "{}",
        message(&document, "daemon")
    );
    assert_eq!(
        check(&document, "daemon")["fix"],
        json!(format!(
            "kill \"$(cat {})\"",
            setup.procly.state_dir().join("daemon.pid").display()
        ))
    );
}

#[test]
fn a_daemon_speaking_another_protocol_version_is_a_problem_and_its_checks_are_skipped() {
    let setup = Setup::new();
    let daemon = fake_daemon(
        setup.procly.state_dir(),
        json!({ "response": "error", "message": "no" }),
        Some(1),
    );

    let document = problems(&setup.doctor());

    daemon.join().expect("the fake Daemon ends");
    assert_eq!(status(&document, "daemon"), "failed");
    assert!(message(&document, "daemon").contains("speaks protocol version 1"));
    assert!(check(&document, "daemon")["fix"].is_string());
    assert_eq!(status(&document, "leased_ports"), "skipped");
    assert_eq!(status(&document, "orphans"), "skipped");
}

#[test]
fn a_daemon_that_cannot_start_is_a_problem_and_the_checks_that_need_none_still_run() {
    let setup = Setup::healthy();
    // A state directory that is a file: the Daemon can neither be reached nor started in it.
    let state = setup.procly.scratch().join("not-a-directory");
    write(setup.procly.scratch(), "not-a-directory", "");

    let output = setup
        .command(&setup.repo)
        .env("PROCLY_STATE_DIR", &state)
        .args(["doctor", "--json"])
        .output()
        .expect("the procly binary runs");

    let document = problems(&output);
    assert_eq!(status(&document, "daemon"), "failed");
    assert_eq!(status(&document, "leased_ports"), "skipped");
    assert_eq!(status(&document, "orphans"), "skipped");
    assert_eq!(status(&document, "docker_compose"), "passed");
    assert_eq!(status(&document, "configuration"), "passed");
}

#[test]
fn a_session_unit_that_is_not_installed_is_a_problem_and_the_session_manager_is_never_asked() {
    let setup = Setup::new();

    let document = problems(&setup.doctor());

    assert_eq!(status(&document, "session_unit"), "failed");
    assert!(message(&document, "session_unit").contains("is not installed"));
    assert_eq!(check(&document, "session_unit")["fix"], json!("procly unit install"));
    assert_eq!(
        setup.unit_calls(),
        "",
        "no unit of this home exists, so the manager has nothing to say about it"
    );
}

#[test]
fn a_session_unit_that_is_not_loaded_is_a_problem() {
    let setup = Setup::healthy();
    std::fs::remove_file(setup.bin.join("loaded")).expect("unload the fake unit");

    let document = problems(&setup.doctor());

    assert_eq!(status(&document, "session_unit"), "failed");
    assert!(message(&document, "session_unit").contains("installed but not loaded"));
    assert_eq!(check(&document, "session_unit")["fix"], json!("procly unit install"));
}

#[test]
fn an_unreadable_state_document_is_a_problem_with_the_command_that_moves_it_aside() {
    let setup = Setup::healthy();
    let leases = setup.procly.state_dir().join("leases.json");
    write(setup.procly.state_dir(), "leases.json", "this is not json");

    let document = problems(&setup.doctor());

    assert_eq!(status(&document, "state_documents"), "failed");
    assert!(
        message(&document, "state_documents").contains(&format!("cannot parse {}", leases.display())),
        "{}",
        message(&document, "state_documents")
    );
    assert_eq!(
        check(&document, "state_documents")["fix"],
        json!(format!("mv {0} {0}.broken", leases.display()))
    );
    // What the Daemon cannot read is no Daemon problem, and the checks that need it say so.
    assert_eq!(status(&document, "daemon"), "passed");
    assert_eq!(status(&document, "leased_ports"), "skipped");
    assert_eq!(status(&document, "orphans"), "skipped");
}

#[test]
fn a_state_document_of_a_newer_procly_is_a_problem_without_a_command_that_would_lose_it() {
    let setup = Setup::healthy();
    write(
        setup.procly.state_dir(),
        "indexes.json",
        r#"{"version":99,"indexes":[]}"#,
    );

    let document = problems(&setup.doctor());

    assert_eq!(status(&document, "state_documents"), "failed");
    assert!(message(&document, "state_documents").contains("written by a newer procly"));
    assert_eq!(check(&document, "state_documents")["fix"], Value::Null);
}

#[test]
fn an_invalid_procly_toml_is_a_problem_with_the_command_that_opens_it() {
    let setup = Setup::healthy();
    write(&setup.repo, "procly.toml", "[services.web]\nbogus = 1\n");

    let document = problems(&setup.doctor());

    assert_eq!(status(&document, "configuration"), "failed");
    assert!(
        message(&document, "configuration").contains("bogus"),
        "{}",
        message(&document, "configuration")
    );
    assert_eq!(
        check(&document, "configuration")["fix"],
        json!(format!("${{EDITOR:-vi}} {}", setup.repo.join("procly.toml").display()))
    );
}

#[test]
fn a_driver_that_does_not_load_is_a_problem_naming_its_service() {
    let setup = Setup::healthy();
    write(
        &setup.repo,
        "procly.toml",
        "[services.db]\ncompose = 'postgres'\ndriver = 'nowhere'\nresources = { dev = { name = 'app' } }\n",
    );

    let document = problems(&setup.doctor());

    assert_eq!(status(&document, "configuration"), "failed");
    assert!(
        message(&document, "configuration").starts_with("service db: "),
        "{}",
        message(&document, "configuration")
    );
    assert!(message(&document, "configuration").contains("nowhere"));
    assert_eq!(
        check(&document, "configuration")["fix"],
        json!(format!(
            "${{EDITOR:-vi}} {}",
            setup.repo.join(".procly/drivers/nowhere.toml").display()
        ))
    );
}

#[test]
fn a_worktree_without_a_procly_toml_and_a_directory_outside_git_skip_the_checks_that_need_them() {
    let setup = Setup::new();
    assert_success(&setup.run(&setup.repo, &["unit", "install"]));

    let in_repository = json_succeeded(&setup.doctor());
    let outside = json_succeeded(&setup.run(setup.procly.scratch(), &["doctor", "--json"]));

    assert_eq!(status(&in_repository, "configuration"), "skipped");
    assert!(message(&in_repository, "configuration").contains("has no procly.toml"));
    assert_eq!(status(&in_repository, "guard"), "skipped");
    for name in ["configuration", "guard"] {
        assert_eq!(status(&outside, name), "skipped", "{name}");
        assert!(
            message(&outside, name).contains("no git Repository"),
            "{}",
            message(&outside, name)
        );
    }
    assert_eq!(status(&outside, "orphans"), "passed");
}

#[test]
fn a_guard_that_left_the_settings_is_a_problem_with_the_command_that_writes_it_again() {
    let setup = Setup::healthy();
    write(&setup.repo.join(".claude"), "settings.json", "{}\n");

    let document = problems(&setup.doctor());

    assert_eq!(status(&document, "guard"), "failed");
    assert!(message(&document, "guard").contains("no longer registers the Guard"));
    assert_eq!(check(&document, "guard")["fix"], json!("procly agent install"));
}

#[test]
fn a_guard_the_user_edited_is_a_problem_that_names_no_command_since_install_leaves_it_alone() {
    let setup = Setup::healthy();
    let settings = setup.repo.join(".claude/settings.json");
    let text = std::fs::read_to_string(&settings).expect("read the settings");
    std::fs::write(&settings, text.replace("\"Bash\"", "\"Bash|Edit\"")).expect("edit the Guard");

    let document = problems(&setup.doctor());

    assert_eq!(status(&document, "guard"), "failed");
    assert!(message(&document, "guard").contains("is not the one Procly recorded"));
    assert_eq!(check(&document, "guard")["fix"], Value::Null);
}

#[test]
fn a_service_that_ended_on_a_port_mismatch_is_a_problem_with_the_command_that_prints_its_lease() {
    let setup = Setup::healthy();
    let (port, actual) = (free_port(), free_port());
    write(
        &setup.repo,
        "procly.toml",
        &format!(
            "[services.web]\nrun = '{} --port {actual}'\nports = {{ http = {port} }}\nhealth = {{ timeout = '10s' }}\n",
            test_server().display()
        ),
    );
    assert_eq!(setup.run(&setup.repo, &["up", "web"]).status.code(), Some(4));

    let document = problems(&setup.doctor());

    assert_eq!(status(&document, "leased_ports"), "failed");
    assert!(
        message(&document, "leased_ports").contains(&format!(
            "service web of {} listened on port {actual} instead of its Lease, port {port} (Port mismatch)",
            setup.repo.display()
        )),
        "{}",
        message(&document, "leased_ports")
    );
    assert_eq!(
        check(&document, "leased_ports")["fix"],
        json!(format!("cd {} && procly env web", setup.repo.display()))
    );
}

/// A dev server started outside Procly, killed when the test ends.
struct StartedOutside(Child);

impl StartedOutside {
    fn start(dir: &Path, port: u16) -> Self {
        let child = Command::new(test_server())
            .args(["--port", &port.to_string()])
            .current_dir(dir)
            .stdout(Stdio::null())
            .spawn()
            .expect("the test server starts");
        eventually("the dev server to listen", || {
            TcpStream::connect(("127.0.0.1", port)).is_ok()
        });
        Self(child)
    }
}

impl Drop for StartedOutside {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn a_foreign_process_on_a_leased_port_is_a_problem_with_the_command_that_stops_it() {
    let setup = Setup::healthy();
    let port = free_port();
    setup.declare_web(port);
    assert_success(&setup.run(&setup.repo, &["up", "web"]));
    assert_success(&setup.run(&setup.repo, &["down", "web"]));
    let server = StartedOutside::start(&setup.repo, port);

    let document = problems(&setup.doctor());

    let pid = server.0.id();
    assert_eq!(status(&document, "leased_ports"), "failed");
    assert!(
        message(&document, "leased_ports").contains(&format!(
            "lease web.http port {port} of {} is held by process {pid} procly-test-server, which Procly did not start",
            setup.repo.display()
        )),
        "{}",
        message(&document, "leased_ports")
    );
    assert_eq!(check(&document, "leased_ports")["fix"], json!(format!("kill {pid}")));
}

#[test]
fn a_service_listening_on_its_own_lease_is_no_foreign_process() {
    let setup = Setup::healthy();
    assert_success(&setup.run(&setup.repo, &["up", "web"]));

    let document = json_succeeded(&setup.doctor());

    assert_eq!(status(&document, "leased_ports"), "passed");
}

#[test]
fn the_orphans_of_a_removed_worktree_are_a_problem_with_the_gc_command_that_lists_them() {
    let setup = Setup::healthy();
    let port = free_port();
    setup.declare_web(port);
    git(&setup.repo, &["add", "procly.toml"]);
    git(&setup.repo, &["commit", "--quiet", "--message", "declare web"]);
    let feature = git_worktree_add(&setup.repo, "app-feature");
    assert_success(&setup.run(&feature, &["up", "web"]));
    assert_success(&setup.run(&feature, &["down", "web"]));
    git(
        &setup.repo,
        &["worktree", "remove", "--force", &feature.to_string_lossy()],
    );

    let output = setup.doctor();

    let document = problems(&output);
    assert_eq!(status(&document, "orphans"), "failed");
    assert!(
        message(&document, "orphans").ends_with("left by removed Worktrees"),
        "{}",
        message(&document, "orphans")
    );
    assert_eq!(check(&document, "orphans")["fix"], json!("procly gc"));
    // Doctor lists, it does not collect: the Orphans are still there for the command it named.
    let gc = setup.run(&setup.repo, &["gc"]);
    assert_success(&gc);
    assert!(stdout(&gc).contains("lease web.http"), "{}", stdout(&gc));
}

#[test]
fn the_text_of_a_problem_gives_its_fix_and_the_exit_code_is_7() {
    let setup = Setup::healthy();
    write(setup.procly.state_dir(), "leases.json", "this is not json");

    let output = setup.run(&setup.repo, &["doctor"]);

    assert_eq!(output.status.code(), Some(7), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("failed  state_documents: cannot parse "), "{text}");
    assert!(text.contains("        fix: mv "), "{text}");
    assert!(text.contains("skipped orphans: "), "{text}");
    assert!(
        stderr(&output).starts_with("procly: 1 check failed, the commands above fix them and doctor ran none"),
        "{}",
        stderr(&output)
    );
}

/// Every file under `dir` with its content, but for the sockets, locks and logs of a running Daemon and the log of
/// the fake session manager.
fn snapshot(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    let mut pending = vec![dir.to_owned()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).expect("read the directory") {
            let path = entry.expect("read the entry").path();
            if path.is_dir() {
                if path.file_name().is_none_or(|name| name != ".git") {
                    pending.push(path);
                }
            } else if path.file_name().is_some_and(|name| name != "calls")
                && !matches!(
                    path.extension().and_then(|extension| extension.to_str()),
                    Some("sock" | "lock" | "log" | "pid")
                )
            {
                files.insert(path.clone(), std::fs::read(&path).unwrap_or_default());
            }
        }
    }
    files
}

#[test]
fn doctor_changes_nothing_in_the_repository_the_state_directory_or_the_home() {
    let setup = Setup::healthy();
    write(setup.procly.state_dir(), "off.json", "this is not json");
    // A first run starts the Daemon, if none runs, as every command does.
    problems(&setup.doctor());
    let scratch = setup.procly.scratch();
    std::fs::remove_file(setup.bin.join("calls")).expect("forget the calls of the installation");
    let before = (snapshot(scratch), snapshot(setup.procly.state_dir()));

    problems(&setup.doctor());
    setup.run(&setup.repo, &["doctor"]);

    assert_eq!(before, (snapshot(scratch), snapshot(setup.procly.state_dir())));
    // It asks the session manager whether the unit is loaded, and never to load or unload it.
    let calls = setup.unit_calls();
    assert!(!calls.is_empty(), "doctor asked the session manager about the unit");
    for write in ["bootstrap", "bootout", "enable ", "disable"] {
        assert!(!calls.contains(write), "doctor ran {write}: {calls}");
    }
}
