//! `procly worktree release`: the Worktree release a worktree manager runs before it removes a Worktree.

mod common;

use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

use common::{
    Procly, assert_success, free_port, git, git_repository, git_worktree_add, json_succeeded, require_docker, stdout,
    test_server,
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
    let directory = output_directory_of(&procly, &other);

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
        format!(
            "web stopped\njob {job} stopped\nlease web.http port {in_other} released\noutput directory {} deleted\n",
            directory.display()
        )
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
    let directory = output_directory_of(&procly, &develop);

    let output = procly.procly_in(&develop, &["worktree", "release"]);

    assert_success(&output);
    assert_eq!(
        stdout(&output),
        format!(
            "web stopped\noutput directory {} deleted\nlease web.http port {leased} kept for Pinned branch develop\n",
            directory.display()
        )
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

    let directory = output_directory_of(&procly, &other);
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
            "retention": [],
            "deleted_output_directory": directory,
            "dry_run": false,
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

/// The bytes of every document the Daemon keeps in its state directory, by file name, for a test that a command
/// changes none of them.
fn documents(procly: &Procly) -> Vec<(&'static str, Option<Vec<u8>>)> {
    [
        "leases.json",
        "indexes.json",
        "resources.json",
        "services.json",
        "containers.json",
        "isolated.json",
        "portless-aliases.json",
    ]
    .into_iter()
    .map(|name| (name, std::fs::read(procly.state_dir().join(name)).ok()))
    .collect()
}

/// The document of a release without its `dry_run` key, the one key a dry run and the release after it tell apart.
fn released_as(mut document: serde_json::Value) -> serde_json::Value {
    document
        .as_object_mut()
        .expect("the document is an object")
        .remove("dry_run");
    document
}

#[test]
fn a_dry_run_changes_nothing_and_a_real_release_then_gives_the_same_report() {
    let procly = Procly::new();
    procly.narrow_range();
    let main = git_repository(procly.scratch(), "app");
    let leftover = procly.scratch().join("leftover.pid");
    let config = format!(
        "{}[services.db]\ncompose = 'postgres'\ndriver = 'postgres'\nports = {{ db = {} }}\n\
         [services.db.resources.dev]\nname = 'app'\n[pinned.develop]\n",
        web_declaring(free_port()),
        free_port()
    );
    let other = git_worktree_add(&main, "app-other");
    write(&other, "procly.toml", &config);
    let in_other = port_of(&procly.procly_in(&other, &["up", "web"]));
    let job = procly.procly_in(
        &other,
        &["run", &format!("sleep 60 & echo $! > {}; wait", leftover.display())],
    );
    assert_success(&job);
    let leftover = common::read_pid(&leftover);
    let develop = worktree_on(&main, "app-develop", "develop", true);
    write(&develop, "procly.toml", &config);
    port_of(&procly.procly_in(&develop, &["up", "web"]));
    let before = documents(&procly);

    let planned_other = json_succeeded(&procly.procly_in(&other, &["worktree", "release", "--dry-run", "--json"]));
    let planned_develop = json_succeeded(&procly.procly_in(&develop, &["worktree", "release", "--dry-run", "--json"]));

    assert_eq!(documents(&procly), before, "a dry run wrote a state file");
    assert!(listens(in_other), "a dry run stopped the Service");
    assert!(common::is_alive(leftover), "a dry run stopped the Job");
    assert_eq!(
        planned_other["stopped"]["services"],
        serde_json::json!(["web"]),
        "{planned_other}"
    );
    assert_eq!(planned_other["stopped"]["jobs"].as_array().map(Vec::len), Some(1));
    assert_eq!(planned_other["released"]["leases"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        planned_other["kept"]["logical_resources"].as_array().map(Vec::len),
        Some(1)
    );
    assert_eq!(planned_develop["kept"]["leases"].as_array().map(Vec::len), Some(1));
    let directory = output_directory_of(&procly, &other);
    assert_eq!(planned_other["deleted_output_directory"], serde_json::json!(directory));
    assert!(directory.is_dir(), "a dry run deleted the output directory");

    // Asking again changes nothing either, and the real release then does what the dry run said.
    assert_eq!(
        json_succeeded(&procly.procly_in(&other, &["worktree", "release", "--dry-run", "--json"])),
        planned_other
    );
    let released_other = json_succeeded(&procly.procly_in(&other, &["worktree", "release", "--json"]));
    let released_develop = json_succeeded(&procly.procly_in(&develop, &["worktree", "release", "--json"]));
    assert_eq!(planned_other["dry_run"], serde_json::json!(true));
    assert_eq!(released_other["dry_run"], serde_json::json!(false));
    assert_eq!(released_as(released_other), released_as(planned_other));
    assert_eq!(released_as(released_develop), released_as(planned_develop));
    assert!(!listens(in_other));
    common::wait_until_gone(leftover);
}

#[test]
fn a_dry_run_says_what_it_would_stop_and_would_release() {
    let procly = Procly::new();
    procly.narrow_range();
    let main = git_repository(procly.scratch(), "app");
    let other = git_worktree_add(&main, "app-other");
    write(&other, "procly.toml", &web_declaring(free_port()));
    let leased = port_of(&procly.procly_in(&other, &["up", "web"]));
    let job = procly.procly_in(&other, &["run", "sleep 60"]);
    assert_success(&job);
    let directory = output_directory_of(&procly, &other).display().to_string();
    let job = stdout(&job).trim().to_owned();

    let output = procly.procly_in(&other, &["worktree", "release", "--dry-run"]);

    assert_success(&output);
    assert_eq!(
        stdout(&output),
        format!(
            "web would stop\njob {job} would stop\nlease web.http port {leased} would release\noutput directory {directory} would delete\n"
        )
    );
    let output = procly.procly_in(&other, &["worktree", "release"]);
    assert_eq!(
        stdout(&output),
        format!(
            "web stopped\njob {job} stopped\nlease web.http port {leased} released\noutput directory {directory} deleted\n"
        )
    );
    // Once released there is nothing left to say it would do.
    let again = procly.procly_in(&other, &["worktree", "release", "--dry-run"]);
    assert_eq!(stdout(&again), "nothing to release\n");
}

#[test]
fn a_dry_run_leaves_a_lease_of_a_running_service_of_another_worktree_and_the_numbered_resources_alone() {
    let procly = Procly::new();
    let (start, _) = procly.narrow_range();
    let main = git_repository(procly.scratch(), "app");
    let used = git_worktree_add(&main, "app-used");
    let config = format!(
        "[services.cache]\ncompose = 'redis'\ndriver = 'redis'\nports = {{ redis = {} }}\n\
         [services.cache.resources.main]\n\
         [services.api]\nrun = '{} --port {{ports.http}}'\nports = {{ http = {} }}\n\
         [services.api.env]\nREDIS_URL = '{{services.cache.resources.main.url}}'\n",
        free_port(),
        test_server().display(),
        free_port()
    );
    for worktree in [&main, &used] {
        write(worktree, "procly.toml", &config);
    }
    assert_success(&procly.procly_in(&used, &["env", "api"]));
    let before = documents(&procly);

    let output = procly.procly_in(&used, &["worktree", "release", "--dry-run"]);

    assert_success(&output);
    assert_eq!(
        stdout(&output),
        format!(
            "lease api.http port {start} would release\n\
             Logical resource 1 of cache (redis) kept, `procly gc` lists it once the Worktree is removed\n"
        )
    );
    assert_eq!(documents(&procly), before, "a dry run wrote a state file");
}

#[test]
fn a_worktree_release_forgets_the_ended_jobs_and_services_of_its_worktree_and_only_those() {
    let procly = Procly::new();
    procly.narrow_range();
    let main = git_repository(procly.scratch(), "app");
    let other = git_worktree_add(&main, "app-other");
    write(&main, "procly.toml", &web_declaring(free_port()));
    write(&other, "procly.toml", &web_declaring(free_port()));
    // Each Worktree has a Service and a Job that have ended, and the Worktree to release also has a Job that runs.
    let ended_job = |dir: &Path| {
        let job = stdout(&procly.procly_in(dir, &["run", "exit 3"])).trim().to_owned();
        assert_eq!(procly.procly(&["wait", &job]).status.code(), Some(3));
        job
    };
    let (released_job, kept_job) = (ended_job(&other), ended_job(&main));
    for dir in [&other, &main] {
        port_of(&procly.procly_in(dir, &["up", "web"]));
        assert_success(&procly.procly_in(dir, &["down", "web"]));
    }
    let running = stdout(&procly.procly_in(&other, &["run", "sleep 60"]))
        .trim()
        .to_owned();

    let before = procly.procly_in(&other, &["status", "--all"]);
    assert!(stdout(&before).contains("service web ended"), "{}", stdout(&before));

    assert_success(&procly.procly_in(&other, &["worktree", "release"]));

    let status = stdout(&procly.procly_in(&other, &["status", "--all"]));
    let worktrees: Vec<&str> = status.split("  worktree ").collect();
    let (main_status, other_status) = (worktrees[1], worktrees[2]);
    assert!(
        main_status.contains("service web ended") && main_status.contains(&kept_job),
        "{status}"
    );
    assert!(
        !other_status.contains("service web") && !other_status.contains(&released_job),
        "{status}"
    );
    assert_eq!(procly.procly(&["wait", &released_job]).status.code(), Some(125));
    assert_eq!(procly.procly(&["wait", &kept_job]).status.code(), Some(3));
    // The release stopped the running Job, which was forgotten with the rest.
    assert_eq!(procly.procly(&["wait", &running]).status.code(), Some(125));
}

#[test]
fn a_dry_run_leaves_the_ended_jobs_and_services_in_place_and_the_release_after_it_clears_them() {
    let procly = Procly::new();
    procly.narrow_range();
    let main = git_repository(procly.scratch(), "app");
    let other = git_worktree_add(&main, "app-other");
    write(&other, "procly.toml", &web_declaring(free_port()));
    let job = stdout(&procly.procly_in(&other, &["run", "exit 3"])).trim().to_owned();
    assert_eq!(procly.procly(&["wait", &job]).status.code(), Some(3));
    port_of(&procly.procly_in(&other, &["up", "web"]));
    assert_success(&procly.procly_in(&other, &["down", "web"]));
    let status = || stdout(&procly.procly_in(&other, &["status"]));
    assert!(status().contains("service web ended"), "{}", status());

    assert_success(&procly.procly_in(&other, &["worktree", "release", "--dry-run"]));

    assert!(
        status().contains("service web ended"),
        "the dry run forgot the Service: {}",
        status()
    );
    assert_eq!(
        procly.procly(&["wait", &job]).status.code(),
        Some(3),
        "the dry run forgot the Job"
    );

    assert_success(&procly.procly_in(&other, &["worktree", "release"]));

    assert!(
        !status().contains("service web"),
        "the release kept the Service: {}",
        status()
    );
    assert_eq!(
        procly.procly(&["wait", &job]).status.code(),
        Some(125),
        "the release kept the Job"
    );
}

#[test]
fn a_worktree_with_no_isolated_container_to_sweep_reports_no_retention_policy() {
    let procly = Procly::new();
    procly.narrow_range();
    let main = git_repository(procly.scratch(), "app");
    let other = git_worktree_add(&main, "app-other");
    write(&other, "procly.toml", &web_declaring(free_port()));
    port_of(&procly.procly_in(&other, &["up", "web"]));

    let text = procly.procly_in(&other, &["worktree", "release", "--dry-run"]);
    let planned = json_succeeded(&procly.procly_in(&other, &["worktree", "release", "--dry-run", "--json"]));

    assert_success(&text);
    assert!(!stdout(&text).contains("Retention policy"), "{}", stdout(&text));
    assert_eq!(planned["retention"], serde_json::json!([]), "{planned}");
    assert_eq!(planned["schema_version"], serde_json::json!(1));
    let released = json_succeeded(&procly.procly_in(&other, &["worktree", "release", "--json"]));
    assert_eq!(released["retention"], serde_json::json!([]), "{released}");
}

/// The output directories of the Worktree `worktree` in the state directory, the ones `procly gc` lists once git no
/// longer lists the Worktree: each records its Worktree in its `worktree.json`.
fn output_directories_of(procly: &Procly, worktree: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(procly.state_dir().join("output")) else {
        return Vec::new();
    };
    let named = serde_json::to_string(worktree).expect("a path serializes");
    entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| std::fs::read_to_string(path.join("worktree.json")).is_ok_and(|record| record.contains(&named)))
        .collect()
}

/// The single output directory of the Worktree `worktree`, which `procly up` created.
fn output_directory_of(procly: &Procly, worktree: &Path) -> PathBuf {
    let found = output_directories_of(procly, worktree);
    assert_eq!(
        found.len(),
        1,
        "one output directory of {}: {found:?}",
        worktree.display()
    );
    found[0].clone()
}

#[test]
fn worktree_release_deletes_the_output_directory_of_the_worktree_and_leaves_no_orphan_of_it() {
    let procly = Procly::new();
    procly.narrow_range();
    let main = git_repository(procly.scratch(), "app");
    let other = git_worktree_add(&main, "app-other");
    write(&main, "procly.toml", &web_declaring(free_port()));
    write(&other, "procly.toml", &web_declaring(free_port()));
    port_of(&procly.procly_in(&main, &["up", "web"]));
    let leased = port_of(&procly.procly_in(&other, &["up", "web"]));
    let (kept, deleted) = (
        output_directory_of(&procly, &main),
        output_directory_of(&procly, &other),
    );

    let output = procly.procly_in(&other, &["worktree", "release"]);

    assert_success(&output);
    assert_eq!(
        stdout(&output),
        format!(
            "web stopped\nlease web.http port {leased} released\noutput directory {} deleted\n",
            deleted.display()
        )
    );
    assert!(!deleted.exists(), "{} is still there", deleted.display());
    assert!(kept.is_dir(), "the output directory of another Worktree went too");
    // Released again, the directory already gone is not named.
    assert_eq!(
        stdout(&procly.procly_in(&other, &["worktree", "release"])),
        "nothing to release\n"
    );
    git(&main, &["worktree", "remove", "--force", &other.to_string_lossy()]);
    let gc = json_succeeded(&procly.procly_in(&main, &["gc", "--json"]));
    let orphans = gc["orphans"].as_array().expect("a list of Orphans");
    assert!(
        !orphans.iter().any(|orphan| orphan["kind"] == "output_directory"),
        "{gc}"
    );
}

#[test]
fn a_dry_run_names_the_output_directory_it_would_delete_and_leaves_it() {
    let procly = Procly::new();
    procly.narrow_range();
    let main = git_repository(procly.scratch(), "app");
    let other = git_worktree_add(&main, "app-other");
    write(&other, "procly.toml", &web_declaring(free_port()));
    let leased = port_of(&procly.procly_in(&other, &["up", "web"]));
    let directory = output_directory_of(&procly, &other);

    let output = procly.procly_in(&other, &["worktree", "release", "--dry-run"]);
    let planned = json_succeeded(&procly.procly_in(&other, &["worktree", "release", "--dry-run", "--json"]));

    assert_success(&output);
    assert_eq!(
        stdout(&output),
        format!(
            "web would stop\nlease web.http port {leased} would release\noutput directory {} would delete\n",
            directory.display()
        )
    );
    assert_eq!(planned["deleted_output_directory"], serde_json::json!(directory));
    assert_eq!(planned["dry_run"], serde_json::json!(true));
    assert!(directory.is_dir(), "a dry run deleted the output directory");

    let released = json_succeeded(&procly.procly_in(&other, &["worktree", "release", "--json"]));

    assert_eq!(released["deleted_output_directory"], serde_json::json!(directory));
    assert_eq!(released["dry_run"], serde_json::json!(false));
    assert!(!directory.exists());
    let again = json_succeeded(&procly.procly_in(&other, &["worktree", "release", "--dry-run", "--json"]));
    assert_eq!(again["deleted_output_directory"], serde_json::Value::Null, "{again}");
}

/// Waits until `path` exists, for at most 10 seconds.
fn wait_for_file(path: &Path) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !path.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "{} never appeared",
            path.display()
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

#[test]
fn an_output_directory_that_cannot_be_deleted_is_named_in_a_warning_and_in_the_log_of_the_daemon() {
    use std::os::unix::fs::PermissionsExt as _;

    let procly = Procly::new();
    procly.narrow_range();
    let main = git_repository(procly.scratch(), "app");
    write(&main, "procly.toml", &web_declaring(free_port()));
    port_of(&procly.procly_in(&main, &["up", "web"]));
    let directory = output_directory_of(&procly, &main);
    // A directory the release cannot empty, inside the output directory the test's own state directory holds.
    let locked = directory.join("locked");
    std::fs::create_dir(&locked).expect("create the locked directory");
    std::fs::write(locked.join("kept.log"), "kept\n").expect("write the kept file");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500)).expect("lock the directory");

    let json = procly.procly_in(&main, &["worktree", "release", "--json"]);
    // Released again, the directory is still there and the deletion fails again, this time in text.
    let text = procly.procly_in(&main, &["worktree", "release"]);
    let log = procly.daemon_log();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).expect("unlock the directory");

    let document = json_succeeded(&json);
    assert_eq!(
        document["deleted_output_directory"],
        serde_json::Value::Null,
        "{document}"
    );
    let named = format!("output directory {} ", directory.display());
    let warning = document["warnings"]
        .as_array()
        .expect("a list of warnings")
        .iter()
        .filter_map(serde_json::Value::as_str)
        .find(|warning| warning.starts_with(&named))
        .unwrap_or_else(|| panic!("no warning names {}: {document}", directory.display()))
        .to_owned();
    assert!(warning.contains("Permission denied"), "{warning}");
    assert_success(&text);
    assert!(
        common::stderr(&text).contains(&named),
        "the warning is not on stderr: {}",
        common::stderr(&text)
    );
    assert!(
        log.lines()
            .any(|line| line.contains(&named) && line.contains("Permission denied")),
        "the Daemon did not log the failure: {log}"
    );
    assert!(locked.join("kept.log").exists());
}

#[test]
fn a_worktree_with_something_still_running_after_the_release_keeps_its_output_directory() {
    let procly = Procly::new();
    procly.narrow_range();
    let main = git_repository(procly.scratch(), "app");
    let other = git_worktree_add(&main, "app-other");
    let (terminated, go) = (procly.scratch().join("terminated"), procly.scratch().join("go"));
    // `web` takes its time to stop: it waits for the test to say so once the release has asked it to.
    let config = format!(
        "[services.web]\nrun = 'trap \"touch {}; while [ ! -f {} ]; do sleep 0.05; done; exit 0\" TERM; {} --port {{ports.http}} & wait'\nports = {{ http = {} }}\n",
        terminated.display(),
        go.display(),
        test_server().display(),
        free_port()
    );
    write(&other, "procly.toml", &config);
    port_of(&procly.procly_in(&other, &["up", "web"]));
    let directory = output_directory_of(&procly, &other);
    let release = procly
        .command()
        .current_dir(&other)
        .args(["worktree", "release", "--json"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("procly worktree release starts");

    // A Job starts in the Worktree while the release waits for `web` to stop, so it still runs once the release is done.
    wait_for_file(&terminated);
    let job = procly.procly_in(&other, &["run", "sleep 60"]);
    assert_success(&job);
    std::fs::write(&go, "").expect("let web stop");
    let released = json_succeeded(&release.wait_with_output().expect("procly worktree release ends"));

    assert_eq!(
        released["deleted_output_directory"],
        serde_json::Value::Null,
        "{released}"
    );
    assert!(
        directory.is_dir(),
        "the output directory went while a Job of the Worktree runs"
    );
}

/// A compose project of a Docker test, whose containers, network and volumes are removed when the test ends.
struct Project(String);

impl Project {
    fn docker(&self, args: &[&str]) -> std::process::Output {
        // A project is found by its name alone, and its Worktree may be removed.
        Command::new("docker")
            .args(["compose", "-p", &self.0])
            .args(args)
            .current_dir(std::env::temp_dir())
            .env("DB_PORT", "1")
            .output()
            .expect("docker runs")
    }

    /// The id of the container of service `db`, running or not, empty when there is none.
    fn container(&self) -> String {
        stdout(&self.docker(&["ps", "-a", "-q", "db"])).trim().to_owned()
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = self.docker(&["down", "--volumes", "--remove-orphans"]);
    }
}

/// A Repository whose service `db` runs one container per Worktree, with a second Worktree that declares the
/// Retention policy `retention`, whose container is left behind stopped when the Worktree is removed.
struct Removed {
    // First, so that the containers are removed while the scratch directory still exists.
    main_project: Project,
    other_project: Project,
    procly: Procly,
    main: PathBuf,
    other: PathBuf,
}

fn removed_with_retention(retention: &str) -> Removed {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let procly = Procly::new();
    procly.narrow_range();
    let name = format!(
        "pcl-rel-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let main = git_repository(procly.scratch(), "app");
    let compose = format!(
        "name: {name}\nservices:\n  db:\n    image: alpine\n    init: true\n    command: [\"nc\", \"-lk\", \"-p\", \
         \"80\", \"-e\", \"/bin/cat\"]\n    ports:\n      - \"${{DB_PORT:-1}}:80\"\n    volumes:\n      - \
         data:/data\nvolumes:\n  data:\n"
    );
    let config = format!(
        "[services.db]\ncompose = \"db\"\nisolation = \"isolated\"\nports = {{ db = {} }}\n\
         env = {{ DB_PORT = \"{{services.db.ports.db}}\" }}\n",
        free_port()
    );
    write(&main, "compose.yaml", &compose);
    write(&main, "procly.toml", &config);
    git(&main, &["add", "."]);
    git(&main, &["commit", "--quiet", "--message", "compose"]);
    let other = git_worktree_add(&main, "app-feature");
    write(&other, "procly.toml", &format!("{config}retention = \"{retention}\"\n"));
    let removed = Removed {
        main_project: Project(name.clone()),
        other_project: Project(format!("{name}-app_feature")),
        procly,
        main,
        other,
    };
    assert_success(&removed.procly.procly_in(&removed.main, &["up", "db"]));
    assert_success(&removed.procly.procly_in(&removed.other, &["up", "db"]));
    assert_success(&removed.procly.procly_in(&removed.other, &["down", "db"]));
    git(
        &removed.main,
        &["worktree", "remove", "--force", &removed.other.to_string_lossy()],
    );
    removed
}

#[test]
fn docker_a_dry_run_reports_the_retention_policy_it_would_apply_and_deletes_nothing() {
    let Some(_docker) = require_docker() else {
        return;
    };
    let removed = removed_with_retention("delete");
    let other = removed.other.display();

    let text = removed
        .procly
        .procly_in(&removed.main, &["worktree", "release", "--dry-run"]);
    let planned = json_succeeded(
        &removed
            .procly
            .procly_in(&removed.main, &["worktree", "release", "--dry-run", "--json"]),
    );

    assert_success(&text);
    assert!(
        stdout(&text).contains(&format!(
            "Retention policy delete of db would apply to the Isolated container of the removed Worktree {other}\n"
        )),
        "{}",
        stdout(&text)
    );
    assert_eq!(
        planned["retention"],
        serde_json::json!([{"worktree": removed.other, "service": "db", "policy": "delete"}]),
        "{planned}"
    );
    assert_eq!(planned["schema_version"], serde_json::json!(1));
    assert!(
        !removed.other_project.container().is_empty(),
        "a dry run deleted the container"
    );
    assert_ne!(
        removed.main_project.container(),
        "",
        "a dry run deleted the container of the Main worktree"
    );
}

#[test]
fn docker_a_dry_run_then_a_real_release_give_the_same_report_retention_policy_included() {
    let Some(_docker) = require_docker() else {
        return;
    };
    let removed = removed_with_retention("delete");
    let release = |args: &[&str]| removed.procly.procly_in(&removed.main, args);

    let planned_text = stdout(&release(&["worktree", "release", "--dry-run"]));
    let planned = json_succeeded(&release(&["worktree", "release", "--dry-run", "--json"]));
    let released = json_succeeded(&release(&["worktree", "release", "--json"]));

    assert_eq!(released_as(released.clone()), released_as(planned));
    assert_eq!(released["retention"].as_array().map(Vec::len), Some(1), "{released}");
    assert!(
        removed.other_project.container().is_empty(),
        "the release kept the container"
    );
    // The text says the same, in the past.
    let line = format!(
        "Retention policy delete of db applied to the Isolated container of the removed Worktree {}",
        removed.other.display()
    );
    assert!(
        planned_text.contains(&line.replace("applied to", "would apply to")),
        "{planned_text}"
    );
    // Deleted, the container has no policy left to apply.
    let again = json_succeeded(&release(&["worktree", "release", "--dry-run", "--json"]));
    assert_eq!(again["retention"], serde_json::json!([]), "{again}");
}

#[test]
fn docker_a_retention_policy_whose_delay_has_not_passed_is_neither_reported_nor_applied() {
    let Some(_docker) = require_docker() else {
        return;
    };
    let removed = removed_with_retention("7d");
    // A Worktree Procly holds nothing in, whose release only finds the removed one waiting for its delay.
    let idle = git_worktree_add(&removed.main, "app-idle");
    let release = |args: &[&str]| removed.procly.procly_in(&idle, args);

    let planned_text = release(&["worktree", "release", "--dry-run"]);
    let planned = json_succeeded(&release(&["worktree", "release", "--dry-run", "--json"]));
    let released_text = release(&["worktree", "release"]);
    let released = json_succeeded(&release(&["worktree", "release", "--json"]));

    assert_success(&planned_text);
    assert_eq!(stdout(&planned_text), "nothing to release\n");
    assert_success(&released_text);
    assert_eq!(stdout(&released_text), "nothing to release\n");
    assert_eq!(planned["retention"], serde_json::json!([]), "{planned}");
    assert_eq!(released["retention"], serde_json::json!([]), "{released}");
    assert!(
        !removed.other_project.container().is_empty(),
        "the container is deleted before its delay"
    );
}

#[test]
fn docker_a_release_that_only_applies_a_retention_policy_does_not_say_it_has_nothing_to_release() {
    let Some(_docker) = require_docker() else {
        return;
    };
    let removed = removed_with_retention("delete");
    // A Worktree Procly holds nothing in, whose release only applies the policy of the removed one.
    let idle = git_worktree_add(&removed.main, "app-idle");
    let line = format!(
        "Retention policy delete of db applied to the Isolated container of the removed Worktree {}\n",
        removed.other.display()
    );

    let planned = removed.procly.procly_in(&idle, &["worktree", "release", "--dry-run"]);
    let released = removed.procly.procly_in(&idle, &["worktree", "release"]);

    assert_success(&planned);
    assert_eq!(stdout(&planned), line.replace("applied to", "would apply to"));
    assert_success(&released);
    assert_eq!(stdout(&released), line);
    assert_eq!(removed.other_project.container(), "", "the release kept the container");
    // Once the policy is applied, there is nothing left to do.
    let again = removed.procly.procly_in(&idle, &["worktree", "release"]);
    assert_eq!(stdout(&again), "nothing to release\n");
}
