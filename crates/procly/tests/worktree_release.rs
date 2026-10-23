//! `procly worktree release`: the Worktree release a worktree manager runs before it removes a Worktree.

mod common;

use std::net::TcpStream;
use std::path::{Path, PathBuf};

use common::{
    Procly, assert_success, free_port, git, git_repository, git_worktree_add, json_succeeded, stdout, test_server,
};

/// Writes `content` to `name` at the root of the Worktree `dir`.
fn write(dir: &Path, name: &str, content: &str) {
    std::fs::write(dir.join(name), content).expect("write the config file");
}

/// A `procly.toml` declaring `web`, which runs the test server on its leased `http` port, declared as `port`.
fn web_declaring(port: u16) -> String {
    format!(
        "[services.web]\nrun = '{} --port {{ports.http}}'\nports = {{ http = {port} }}\n",
        test_server().display()
    )
}

/// The port of the health check `procly up` printed, at the end of the first Lease of its line.
fn port_of(output: &std::process::Output) -> u16 {
    assert_success(output);
    let line = stdout(output);
    line.trim_end()
        .split(", ")
        .next()
        .unwrap_or_default()
        .rsplit(' ')
        .next()
        .and_then(|port| port.parse().ok())
        .unwrap_or_else(|| panic!("no port at the end of {line:?}"))
}

/// Adds a Worktree next to the Main worktree on `branch`, created when `create`, and returns it canonicalized.
fn worktree_on(main: &Path, name: &str, branch: &str, create: bool) -> PathBuf {
    let path = main.parent().expect("the Main worktree has a parent").join(name);
    let path_arg = path.to_string_lossy().into_owned();
    if create {
        git(main, &["worktree", "add", "--quiet", "-b", branch, &path_arg]);
    } else {
        git(main, &["worktree", "add", "--quiet", &path_arg, branch]);
    }
    path.canonicalize().expect("canonicalize the worktree")
}

fn listens(port: u16) -> bool {
    TcpStream::connect(("127.0.0.1", port)).is_ok()
}

#[test]
fn worktree_release_stops_every_service_and_job_of_the_worktree_with_their_trees_and_the_daemon_logs_who_asked() {
    let procly = Procly::new();
    procly.narrow_range();
    let main = git_repository(procly.scratch(), "app");
    let other = git_worktree_add(&main, "app-other");
    let sleeper = procly.scratch().join("sleeper.pid");
    let leftover = procly.scratch().join("leftover.pid");
    // The sleep is a second member of the group, it goes only if the whole group is signalled.
    let config = format!(
        "[services.web]\nrun = 'sleep 60 & echo $! > {}; exec {} --port {{ports.http}}'\nports = {{ http = {} }}\n",
        sleeper.display(),
        test_server().display(),
        free_port()
    );
    write(&main, "procly.toml", &config);
    write(&other, "procly.toml", &config);
    let in_main = port_of(&procly.procly_in(&main, &["up", "web"]));
    let in_other = port_of(&procly.procly_in(&other, &["up", "web"]));
    let sleeper = common::read_pid(&sleeper);
    let job = procly.procly_in(
        &other,
        &["run", &format!("sleep 60 & echo $! > {}; wait", leftover.display())],
    );
    assert_success(&job);
    let job = stdout(&job).trim().to_owned();
    let leftover = common::read_pid(&leftover);

    let release = procly
        .command()
        .current_dir(&other)
        .args(["worktree", "release"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("procly worktree release starts");
    let requester = release.id();
    let output = release.wait_with_output().expect("procly worktree release ends");

    assert_success(&output);
    assert_eq!(
        stdout(&output),
        format!("web stopped\njob {job} stopped\nlease web.http port {in_other} released\n")
    );
    common::wait_until_gone(sleeper);
    common::wait_until_gone(leftover);
    assert!(!listens(in_other), "the server of the released Worktree still listens");
    // The other Worktree keeps its Service.
    assert_eq!(common::http_get(in_main, "/"), "ok\n");
    let log = procly.daemon_log();
    assert!(
        log.contains(&format!(
            "worktree release of {} requested by pid {requester}",
            other.display()
        )),
        "{log}"
    );
}

#[test]
fn the_leases_of_a_released_worktree_are_granted_at_once_to_a_new_worktree() {
    // The clock stands still: the Leases are free without waiting for them to expire.
    let procly = Procly::with_clock();
    let (start, _) = procly.narrow_range();
    let main = git_repository(procly.scratch(), "app");
    let config = web_declaring(free_port());
    let first = git_worktree_add(&main, "app-first");
    write(&first, "procly.toml", &config);
    assert_eq!(port_of(&procly.procly_in(&first, &["up", "web"])), start);

    assert_success(&procly.procly_in(&first, &["worktree", "release"]));
    git(&main, &["worktree", "remove", "--force", &first.to_string_lossy()]);
    let second = git_worktree_add(&main, "app-second");
    write(&second, "procly.toml", &config);

    assert_eq!(port_of(&procly.procly_in(&second, &["up", "web"])), start);
}

#[test]
fn the_leases_of_a_pinned_branch_are_kept_and_its_recreated_worktree_gets_the_same_ports() {
    let procly = Procly::new();
    procly.narrow_range();
    let main = git_repository(procly.scratch(), "app");
    let config = format!("{}[pinned.develop]\n", web_declaring(free_port()));
    let develop = worktree_on(&main, "app-develop", "develop", true);
    write(&develop, "procly.toml", &config);
    let leased = port_of(&procly.procly_in(&develop, &["up", "web"]));

    let output = procly.procly_in(&develop, &["worktree", "release"]);

    assert_success(&output);
    assert_eq!(
        stdout(&output),
        format!("web stopped\nlease web.http port {leased} kept for Pinned branch develop\n")
    );
    git(&main, &["worktree", "remove", "--force", &develop.to_string_lossy()]);
    // Another Worktree asks in between, it would get the port of a Lease the release had let go of.
    let other = git_worktree_add(&main, "app-other");
    write(&other, "procly.toml", &config);
    assert_ne!(port_of(&procly.procly_in(&other, &["up", "web"])), leased);
    let recreated = worktree_on(&main, "app-develop-again", "develop", false);
    write(&recreated, "procly.toml", &config);
    assert_eq!(port_of(&procly.procly_in(&recreated, &["up", "web"])), leased);
}

#[test]
fn releasing_twice_or_in_a_worktree_procly_never_saw_has_nothing_to_release() {
    let procly = Procly::new();
    procly.narrow_range();
    let main = git_repository(procly.scratch(), "app");
    let other = git_worktree_add(&main, "app-other");
    write(&other, "procly.toml", &web_declaring(free_port()));
    assert_success(&procly.procly_in(&other, &["up", "web"]));
    assert_success(&procly.procly_in(&other, &["worktree", "release"]));
    let unseen = git_worktree_add(&main, "app-unseen");

    for worktree in [&other, &unseen] {
        let output = procly.procly_in(worktree, &["worktree", "release"]);
        assert_success(&output);
        assert_eq!(stdout(&output), "nothing to release\n");
    }
}

#[test]
fn a_worktree_release_names_the_logical_resources_it_keeps_and_the_command_that_lists_them() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let other = git_worktree_add(&main, "app-other");
    let config = format!(
        "[services.db]\ncompose = 'postgres'\ndriver = 'postgres'\nports = {{ db = {} }}\n\
         [services.db.resources.dev]\nname = 'app'\n\
         [services.db.resources.test]\nname = 'app_test'\n",
        free_port()
    );
    write(&other, "procly.toml", &config);

    let output = procly.procly_in(&other, &["worktree", "release"]);

    assert_success(&output);
    assert_eq!(
        stdout(&output),
        "nothing to release\n\
         Logical resource app__app_other of db (postgres) kept, `procly gc` lists it once the Worktree is removed\n\
         Logical resource app_test__app_other of db (postgres) kept, `procly gc` lists it once the Worktree is removed\n"
    );
}

#[test]
fn a_logical_resource_of_a_driver_that_numbers_them_is_named_once_the_worktree_holds_its_index() {
    let procly = Procly::new();
    let (start, _) = procly.narrow_range();
    let main = git_repository(procly.scratch(), "app");
    let (used, unused) = (
        git_worktree_add(&main, "app-used"),
        git_worktree_add(&main, "app-unused"),
    );
    let config = format!(
        "[services.cache]\ncompose = 'redis'\ndriver = 'redis'\nports = {{ redis = {} }}\n\
         [services.cache.resources.main]\n\
         [services.api]\nrun = '{} --port {{ports.http}}'\nports = {{ http = {} }}\n\
         [services.api.env]\nREDIS_URL = '{{services.cache.resources.main.url}}'\n",
        free_port(),
        test_server().display(),
        free_port()
    );
    for worktree in [&main, &used, &unused] {
        write(worktree, "procly.toml", &config);
    }
    // Reading the URL hands out the index, 0 going to the Main worktree, and grants the Lease of `api`.
    assert_success(&procly.procly_in(&used, &["env", "api"]));

    let output = procly.procly_in(&used, &["worktree", "release"]);

    assert_success(&output);
    assert_eq!(
        stdout(&output),
        format!(
            "lease api.http port {start} released\n\
             Logical resource 1 of cache (redis) kept, `procly gc` lists it once the Worktree is removed\n"
        )
    );
    let output = procly.procly_in(&unused, &["worktree", "release"]);
    assert_eq!(stdout(&output), "nothing to release\n");
}

#[test]
fn worktree_release_json_reports_what_was_stopped_released_and_kept() {
    let procly = Procly::new();
    procly.narrow_range();
    let main = git_repository(procly.scratch(), "app");
    let config = format!(
        "{}[services.db]\ncompose = 'postgres'\ndriver = 'postgres'\nports = {{ db = {} }}\n\
         [services.db.resources.dev]\nname = 'app'\n[pinned.develop]\n",
        web_declaring(free_port()),
        free_port()
    );
    let other = git_worktree_add(&main, "app-other");
    write(&other, "procly.toml", &config);
    let in_other = port_of(&procly.procly_in(&other, &["up", "web"]));
    let develop = worktree_on(&main, "app-develop", "develop", true);
    write(&develop, "procly.toml", &config);
    let in_develop = port_of(&procly.procly_in(&develop, &["up", "web"]));

    let released = json_succeeded(&procly.procly_in(&other, &["worktree", "release", "--json"]));
    let kept = json_succeeded(&procly.procly_in(&develop, &["worktree", "release", "--json"]));

    assert_eq!(
        released,
        serde_json::json!({
            "schema_version": 1,
            "worktree": other,
            "stopped": {"services": ["web"], "jobs": []},
            "released": {
                "compose_services": [],
                "leases": [{"service": "web", "port_name": "http", "port": in_other}],
            },
            "kept": {
                "leases": [],
                "logical_resources": [
                    {"service": "db", "driver": "postgres", "resource": "dev", "name": "app__app_other"},
                ],
            },
            "warnings": [],
        })
    );
    assert_eq!(kept["released"]["leases"], serde_json::json!([]));
    assert_eq!(
        kept["kept"]["leases"],
        serde_json::json!([
            {"service": "web", "port_name": "http", "port": in_develop, "reason": "pinned_branch", "branch": "develop"},
        ])
    );
}
