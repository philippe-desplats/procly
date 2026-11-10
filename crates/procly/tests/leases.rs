mod common;

use std::path::Path;

use std::time::Duration;

use common::{Procly, assert_success, free_port, git, git_repository, stdout, test_server};

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

#[test]
fn the_main_worktree_gets_the_declared_port_through_the_command_placeholder() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let port = free_port();
    write(&main, "procly.toml", &web_declaring(port));

    let output = procly.procly_in(&main, &["up", "web"]);

    assert_success(&output);
    assert_eq!(
        stdout(&output),
        format!("web started http://localhost:{port} port {port}\n")
    );
    assert_eq!(common::http_get(port, "/"), "ok\n");
}

#[test]
fn a_second_worktree_gets_a_port_from_the_range_and_not_the_declared_one() {
    let procly = Procly::new();
    let (start, end) = procly.narrow_range();
    let main = git_repository(procly.scratch(), "app");
    let other = common::git_worktree_add(&main, "app-other");
    let declared = free_port();
    write(&main, "procly.toml", &web_declaring(declared));
    write(&other, "procly.toml", &web_declaring(declared));

    let in_main = port_of(&procly.procly_in(&main, &["up", "web"]));
    let in_other = port_of(&procly.procly_in(&other, &["up", "web"]));

    assert_eq!(in_main, declared);
    assert!((start..=end).contains(&in_other), "{in_other} is outside {start}-{end}");
    assert_eq!(common::http_get(in_other, "/"), "ok\n");
}

#[test]
fn a_port_held_by_a_foreign_process_is_skipped() {
    let procly = Procly::new();
    let (start, _) = procly.narrow_range();
    let main = git_repository(procly.scratch(), "app");
    let other = common::git_worktree_add(&main, "app-other");
    write(&other, "procly.toml", &web_declaring(free_port()));
    let foreign = std::net::TcpListener::bind(("127.0.0.1", start)).expect("hold the first port of the Range");

    let port = port_of(&procly.procly_in(&other, &["up", "web"]));

    assert_ne!(port, start);
    drop(foreign);
}

#[test]
fn leases_stay_the_same_across_restarts_of_the_service_and_of_the_daemon() {
    let procly = Procly::new();
    procly.narrow_range();
    let main = git_repository(procly.scratch(), "app");
    let (first, second) = (
        common::git_worktree_add(&main, "app-first"),
        common::git_worktree_add(&main, "app-second"),
    );
    for worktree in [&first, &second] {
        write(worktree, "procly.toml", &web_declaring(free_port()));
    }
    let in_first = port_of(&procly.procly_in(&first, &["up", "web"]));
    let in_second = port_of(&procly.procly_in(&second, &["up", "web"]));

    assert_success(&procly.procly_in(&second, &["down", "web"]));
    assert_eq!(port_of(&procly.procly_in(&second, &["up", "web"])), in_second);

    procly.stop_daemon();
    // The second Worktree asks first, a Daemon that forgot would hand it the port of the first one.
    assert_eq!(port_of(&procly.procly_in(&second, &["up", "web"])), in_second);
    assert_eq!(port_of(&procly.procly_in(&first, &["up", "web"])), in_first);
    let document = std::fs::metadata(procly.state_dir().join("leases.json")).expect("the Leases are stored");
    assert_eq!(
        std::os::unix::fs::PermissionsExt::mode(&document.permissions()) & 0o777,
        0o600
    );
}

#[test]
fn concurrent_ups_from_several_worktrees_never_get_the_same_port() {
    let procly = Procly::new();
    procly.narrow_range();
    let main = git_repository(procly.scratch(), "app");
    let worktrees: Vec<_> = (0..4)
        .map(|i| common::git_worktree_add(&main, &format!("app-{i}")))
        .collect();
    for worktree in &worktrees {
        write(worktree, "procly.toml", &web_declaring(free_port()));
    }

    let ups: Vec<_> = worktrees
        .iter()
        .map(|worktree| {
            procly
                .command()
                .current_dir(worktree)
                .args(["up", "web"])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .expect("procly up starts")
        })
        .collect();
    let mut ports: Vec<u16> = ups
        .into_iter()
        .map(|up| port_of(&up.wait_with_output().expect("procly up ends")))
        .collect();

    ports.sort_unstable();
    ports.dedup();
    assert_eq!(ports.len(), 4, "{ports:?}");
}

#[test]
fn an_unreadable_or_newer_lease_document_fails_up_with_125_naming_it() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    write(&main, "procly.toml", &web_declaring(free_port()));
    let document = procly.state_dir().join("leases.json");

    for (content, why) in [
        ("{\"version\": 1, \"leases\": [", "cannot parse"),
        ("{\"version\": 3, \"leases\": []}", "written by a newer procly"),
    ] {
        std::fs::write(&document, content).unwrap();

        let output = procly.procly_in(&main, &["up", "web"]);

        assert_eq!(output.status.code(), Some(125));
        let stderr = common::stderr(&output);
        assert!(stderr.contains("leases.json") && stderr.contains(why), "{stderr}");
    }
}

#[test]
fn each_port_name_gets_its_own_lease_injected_as_port_and_a_per_name_variable() {
    let procly = Procly::new();
    let (start, end) = procly.narrow_range();
    let main = git_repository(procly.scratch(), "app");
    let other = common::git_worktree_add(&main, "app-other");
    let env = procly.scratch().join("env");
    let config = format!(
        "[services.web]\nrun = 'echo \"$PORT $PORT_HTTP $PORT_TEST {{ports.test}}\" > {}; exec {} --port {{ports.http}}'\n\
         ports = {{ http = {}, test = {} }}\nhealth = {{ port = 'http' }}\n",
        env.display(),
        test_server().display(),
        free_port(),
        free_port()
    );
    write(&other, "procly.toml", &config);

    let http = port_of(&procly.procly_in(&other, &["up", "web"]));

    let env = std::fs::read_to_string(&env).expect("the service wrote its environment");
    let values: Vec<u16> = env.split_whitespace().map(|port| port.parse().unwrap()).collect();
    let [port, port_http, port_test, placeholder] = values[..] else {
        panic!("unexpected environment {env:?}")
    };
    assert_eq!((port, port_http), (http, http));
    assert_eq!(placeholder, port_test);
    assert_ne!(port_test, http);
    assert!(
        (start..=end).contains(&port_test),
        "{port_test} is outside {start}-{end}"
    );
}

/// A `procly.toml` declaring `web` with `port` for `http`, whose command listens on `actual` instead.
fn web_listening_elsewhere(port: u16, actual: u16) -> String {
    format!(
        "[services.web]\nrun = '{} --port {actual}'\nports = {{ http = {port} }}\nhealth = {{ timeout = '1s' }}\n",
        test_server().display()
    )
}

#[test]
fn a_service_listening_on_another_port_is_a_port_mismatch_exiting_4() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let (port, actual) = (free_port(), free_port());
    write(&main, "procly.toml", &web_listening_elsewhere(port, actual));

    let output = procly.procly_in(&main, &["up", "web"]);

    assert_eq!(output.status.code(), Some(4), "stderr: {}", common::stderr(&output));
    assert!(
        common::stderr(&output).starts_with(&format!(
            "procly: service web listens on port {actual} instead of its Lease, port {port} (Port mismatch), its \
             process tree was stopped, its recent output:\n"
        )),
        "{}",
        common::stderr(&output)
    );
    assert!(std::net::TcpStream::connect(("127.0.0.1", actual)).is_err());
}

#[test]
fn a_foreign_process_answering_on_the_lease_is_no_health_when_the_service_listens_elsewhere() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let port = free_port();
    write(&main, "procly.toml", &web_declaring(port));
    assert_success(&procly.procly_in(&main, &["up", "web"]));
    assert_success(&procly.procly_in(&main, &["down", "web"]));
    let foreign = std::net::TcpListener::bind(("127.0.0.1", port)).expect("hold the Lease");
    let actual = free_port();
    // The server opens its port only after a while, as a dev server compiling first, while the Foreign one answers.
    let config = format!(
        "[services.web]\nrun = 'sleep 1; exec {} --port {actual}'\nports = {{ http = {port} }}\n\
         health = {{ timeout = '5s' }}\n",
        test_server().display()
    );
    write(&main, "procly.toml", &config);

    let output = procly.procly_in(&main, &["up", "web"]);

    assert_eq!(output.status.code(), Some(4), "stderr: {}", common::stderr(&output));
    assert!(
        common::stderr(&output).contains(&format!("listens on port {actual} instead of its Lease, port {port}")),
        "{}",
        common::stderr(&output)
    );
    drop(foreign);
}

/// A `procly.toml` declaring `web` like [`web_declaring`], with `pool` as the Pool of `http`, and `extra` after it.
fn web_with_pool(port: u16, pool: &[u16], extra: &str) -> String {
    let pool: Vec<String> = pool.iter().map(u16::to_string).collect();
    format!(
        "[services.web]\nrun = '{} --port {{ports.http}}'\nports = {{ http = {{ port = {port}, pool = [{}] }} }}\n{extra}",
        test_server().display(),
        pool.join(", ")
    )
}

/// Adds a Worktree next to the Main worktree on `branch`, created when `create`, and returns it canonicalized.
fn worktree_on(main: &Path, name: &str, branch: &str, create: bool) -> std::path::PathBuf {
    let path = main.parent().expect("the Main worktree has a parent").join(name);
    let path_arg = path.to_string_lossy().into_owned();
    if create {
        git(main, &["worktree", "add", "--quiet", "-b", branch, &path_arg]);
    } else {
        git(main, &["worktree", "add", "--quiet", &path_arg, branch]);
    }
    path.canonicalize().expect("canonicalize the worktree")
}

#[test]
fn a_secondary_worktree_gets_the_first_free_port_of_the_pool_before_the_range() {
    let procly = Procly::new();
    procly.narrow_range();
    let main = git_repository(procly.scratch(), "app");
    let other = common::git_worktree_add(&main, "app-other");
    let (declared, taken, free) = (free_port(), free_port(), free_port());
    let config = web_with_pool(declared, &[taken, free], "");
    write(&main, "procly.toml", &config);
    write(&other, "procly.toml", &config);
    let foreign = std::net::TcpListener::bind(("127.0.0.1", taken)).expect("hold the first port of the Pool");

    assert_eq!(port_of(&procly.procly_in(&main, &["up", "web"])), declared);
    assert_eq!(port_of(&procly.procly_in(&other, &["up", "web"])), free);
    assert_eq!(common::http_get(free, "/"), "ok\n");
    drop(foreign);
}

#[test]
fn once_the_pool_is_exhausted_a_worktree_gets_a_port_from_the_range() {
    let procly = Procly::new();
    let (start, end) = procly.narrow_range();
    let main = git_repository(procly.scratch(), "app");
    let (first, second) = (
        common::git_worktree_add(&main, "app-first"),
        common::git_worktree_add(&main, "app-second"),
    );
    let pooled = free_port();
    let config = web_with_pool(free_port(), &[pooled], "");
    write(&first, "procly.toml", &config);
    write(&second, "procly.toml", &config);

    assert_eq!(port_of(&procly.procly_in(&first, &["up", "web"])), pooled);
    let in_second = port_of(&procly.procly_in(&second, &["up", "web"]));

    assert!(
        (start..=end).contains(&in_second),
        "{in_second} is outside {start}-{end}"
    );
}

#[test]
fn a_worktree_of_a_pinned_branch_removed_and_recreated_elsewhere_gets_the_same_leases() {
    let procly = Procly::new();
    procly.narrow_range();
    let main = git_repository(procly.scratch(), "app");
    let config = format!("{}[pinned.develop]\n", web_declaring(free_port()));
    let develop = worktree_on(&main, "app-develop", "develop", true);
    write(&develop, "procly.toml", &config);
    let leased = port_of(&procly.procly_in(&develop, &["up", "web"]));
    assert_success(&procly.procly_in(&develop, &["down", "web"]));
    git(&main, &["worktree", "remove", "--force", &develop.to_string_lossy()]);

    // Another Worktree asks in between, it would get the port of a Lease that went with the removed Worktree.
    let other = common::git_worktree_add(&main, "app-other");
    write(&other, "procly.toml", &config);
    assert_ne!(port_of(&procly.procly_in(&other, &["up", "web"])), leased);
    let recreated = worktree_on(&main, "app-develop-again", "develop", false);
    write(&recreated, "procly.toml", &config);

    assert_eq!(port_of(&procly.procly_in(&recreated, &["up", "web"])), leased);
}

#[test]
fn the_explicit_ports_of_a_pinned_branch_are_honoured() {
    let procly = Procly::new();
    procly.narrow_range();
    let main = git_repository(procly.scratch(), "app");
    let explicit = free_port();
    let config = format!(
        "{}[pinned.staging.ports]\nweb = {{ http = {explicit} }}\n",
        web_declaring(free_port())
    );
    let staging = worktree_on(&main, "app-staging", "staging", true);
    let other = common::git_worktree_add(&main, "app-other");
    write(&staging, "procly.toml", &config);
    write(&other, "procly.toml", &config);

    assert_ne!(port_of(&procly.procly_in(&other, &["up", "web"])), explicit);
    assert_eq!(port_of(&procly.procly_in(&staging, &["up", "web"])), explicit);
    assert_eq!(common::http_get(explicit, "/"), "ok\n");
}

#[test]
fn an_inactive_lease_expires_after_30_days_and_a_pinned_branch_lease_never_does() {
    let procly = Procly::with_clock();
    procly.narrow_range();
    let main = git_repository(procly.scratch(), "app");
    let config = format!("{}[pinned.develop]\n", web_declaring(free_port()));
    let worktree = |name: &str| {
        let worktree = common::git_worktree_add(&main, name);
        write(&worktree, "procly.toml", &config);
        worktree
    };
    let up_down = |worktree: &Path| {
        let port = port_of(&procly.procly_in(worktree, &["up", "web"]));
        assert_success(&procly.procly_in(worktree, &["down", "web"]));
        port
    };
    let abandoned = worktree("app-abandoned");
    let develop = worktree_on(&main, "app-develop", "develop", true);
    write(&develop, "procly.toml", &config);
    let in_abandoned = up_down(&abandoned);
    let in_develop = up_down(&develop);

    procly.advance_clock(Duration::from_hours(29 * 24));
    let recent = worktree("app-recent");
    let in_recent = up_down(&recent);
    assert!(
        ![in_abandoned, in_develop].contains(&in_recent),
        "{in_recent} was still leased"
    );

    // 30 days since the abandoned Worktree used its Lease, 1 day since the recent one did.
    procly.advance_clock(Duration::from_hours(24));
    assert_eq!(up_down(&worktree("app-new")), in_abandoned);
    let later = up_down(&worktree("app-later"));
    assert!(![in_develop, in_recent].contains(&later), "{later} was still leased");
    assert_eq!(up_down(&develop), in_develop);
}

#[test]
fn a_lease_document_of_version_1_is_read_and_its_ports_kept() {
    let procly = Procly::new();
    procly.narrow_range();
    let main = git_repository(procly.scratch(), "app");
    let other = common::git_worktree_add(&main, "app-other");
    write(&other, "procly.toml", &web_declaring(free_port()));
    let leased = free_port();
    let document = format!(
        "{{\"version\": 1, \"leases\": [{{\"repository\": {:?}, \"worktree\": {:?}, \"service\": \"web\", \
         \"port_name\": \"http\", \"port\": {leased}}}]}}\n",
        main.join(".git").canonicalize().unwrap(),
        other
    );
    std::fs::write(procly.state_dir().join("leases.json"), document).unwrap();

    assert_eq!(port_of(&procly.procly_in(&other, &["up", "web"])), leased);
}

#[test]
fn a_lease_counts_as_used_until_its_service_ends() {
    let procly = Procly::with_clock();
    procly.narrow_range();
    let main = git_repository(procly.scratch(), "app");
    // Never stopped for being idle, the clock jumps by days while it runs.
    let config = format!("{}idle = {{ stop = false }}\n", web_declaring(free_port()));
    let worktree = |name: &str| {
        let worktree = common::git_worktree_add(&main, name);
        write(&worktree, "procly.toml", &config);
        worktree
    };
    let long_running = worktree("app-long");
    let leased = port_of(&procly.procly_in(&long_running, &["up", "web"]));

    procly.advance_clock(Duration::from_hours(29 * 24));
    assert_success(&procly.procly_in(&long_running, &["down", "web"]));
    // 31 days since the Lease was granted, 2 days since its Service ended.
    procly.advance_clock(Duration::from_hours(2 * 24));
    let other = worktree("app-other");

    assert_ne!(port_of(&procly.procly_in(&other, &["up", "web"])), leased);
}

#[test]
fn the_main_worktree_on_a_pinned_branch_keeps_its_declared_port_on_any_branch() {
    let procly = Procly::new();
    procly.narrow_range();
    let main = git_repository(procly.scratch(), "app");
    let declared = free_port();
    write(
        &main,
        "procly.toml",
        &format!("{}[pinned.develop]\n", web_declaring(declared)),
    );
    git(&main, &["checkout", "--quiet", "-b", "develop"]);
    assert_eq!(port_of(&procly.procly_in(&main, &["up", "web"])), declared);
    assert_success(&procly.procly_in(&main, &["down", "web"]));

    git(&main, &["checkout", "--quiet", "-b", "feature"]);

    assert_eq!(port_of(&procly.procly_in(&main, &["up", "web"])), declared);
}

/// A `procly.toml` declaring `api` on port `api` and `admin` on port `admin`, each running the test server on its
/// leased `http` port.
fn api_and_admin_declaring(api: u16, admin: u16) -> String {
    let server = test_server();
    format!(
        "[services.api]\nrun = '{server} --port {{ports.http}}'\nports = {{ http = {api} }}\n\n\
         [services.admin]\nrun = '{server} --port {{ports.http}}'\nports = {{ http = {admin} }}\n",
        server = server.display()
    )
}

#[test]
fn crossed_leases_of_stopped_services_each_follow_their_new_declaration_in_either_order() {
    for order in [["admin", "api"], ["api", "admin"]] {
        let procly = Procly::new();
        let main = git_repository(procly.scratch(), "app");
        let (first, second) = (free_port(), free_port());
        write(&main, "procly.toml", &api_and_admin_declaring(first, second));
        assert_eq!(port_of(&procly.procly_in(&main, &["up", "api"])), first);
        assert_eq!(port_of(&procly.procly_in(&main, &["up", "admin"])), second);
        assert_success(&procly.procly_in(&main, &["down"]));

        // The declared ports were swapped by mistake, and are swapped back while nothing runs.
        write(&main, "procly.toml", &api_and_admin_declaring(second, first));

        for service in order {
            let expected = if service == "api" { second } else { first };
            assert_eq!(
                port_of(&procly.procly_in(&main, &["up", service])),
                expected,
                "{service} brought up in the order {order:?}"
            );
            assert_eq!(common::http_get(expected, "/"), "ok\n");
        }
        let log = procly.daemon_log();
        assert!(
            log.contains("yielded its port")
                && log.contains(&format!("port {}", order_first_port(order, first, second))),
            "the Daemon logs the Lease that yielded its port: {log}"
        );
    }
}

/// The port the Service brought up first in `order` takes from the stale Lease of the other one.
fn order_first_port(order: [&str; 2], admin: u16, api: u16) -> u16 {
    if order[0] == "admin" { admin } else { api }
}

#[test]
fn procly_env_moves_the_lease_of_a_stopped_service_to_its_new_declared_port() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let (old, new) = (free_port(), free_port());
    write(&main, "procly.toml", &web_declaring(old));
    assert_eq!(port_of(&procly.procly_in(&main, &["up", "web"])), old);
    assert_success(&procly.procly_in(&main, &["down", "web"]));
    write(&main, "procly.toml", &web_declaring(new));

    let env = procly.procly_in(&main, &["env", "web"]);

    assert_success(&env);
    assert!(
        stdout(&env).contains(&format!("PORT='{new}'")),
        "env carries the new declared port: {}",
        stdout(&env)
    );
    assert_eq!(port_of(&procly.procly_in(&main, &["up", "web"])), new);
}

#[test]
fn a_new_declared_port_held_by_a_process_fails_up_naming_it_and_keeps_the_lease_until_it_is_freed() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let (old, new) = (free_port(), free_port());
    write(&main, "procly.toml", &web_declaring(old));
    assert_eq!(port_of(&procly.procly_in(&main, &["up", "web"])), old);
    assert_success(&procly.procly_in(&main, &["down", "web"]));
    write(&main, "procly.toml", &web_declaring(new));
    let foreign = std::net::TcpListener::bind(("127.0.0.1", new)).expect("hold the new declared port");

    let refused = procly.procly_in(&main, &["up", "web"]);

    assert_eq!(refused.status.code(), Some(125), "{}", common::stderr(&refused));
    let message = common::stderr(&refused);
    assert!(message.contains(&format!("declared port {new}")), "{message}");
    assert!(
        message.contains(&format!("process {}", std::process::id())),
        "{message}"
    );
    let stop = message
        .find(&format!("{} {}", "kill", std::process::id()))
        .unwrap_or_else(|| panic!("the command that stops the process: {message}"));
    // Releasing the Leases of the Main worktree stops nothing outside Procly, so it comes first.
    let release = message
        .find("procly worktree release")
        .unwrap_or_else(|| panic!("the release of the Main worktree: {message}"));
    assert!(release < stop, "{message}");
    assert!(message.contains(&format!("port {old}")), "{message}");
    // The refusal survives a restart of the Daemon, the declaration it was granted under is stored with the Lease.
    procly.stop_daemon();
    assert_eq!(procly.procly_in(&main, &["up", "web"]).status.code(), Some(125));

    drop(foreign);
    assert_eq!(port_of(&procly.procly_in(&main, &["up", "web"])), new);
}

#[test]
fn a_declared_port_taken_at_the_first_grant_stays_a_fallback_across_restarts_of_the_daemon() {
    let procly = Procly::new();
    let (start, end) = procly.narrow_range();
    let main = git_repository(procly.scratch(), "app");
    let declared = free_port();
    write(&main, "procly.toml", &web_declaring(declared));
    let foreign = std::net::TcpListener::bind(("127.0.0.1", declared)).expect("hold the declared port");
    let fallback = port_of(&procly.procly_in(&main, &["up", "web"]));
    assert!((start..=end).contains(&fallback), "{fallback} is outside {start}-{end}");
    assert_success(&procly.procly_in(&main, &["down", "web"]));

    procly.stop_daemon();

    // The declaration did not change, so the Lease is not refused, it stays where the first grant put it.
    assert_eq!(port_of(&procly.procly_in(&main, &["up", "web"])), fallback);
    assert_success(&procly.procly_in(&main, &["down", "web"]));
    assert_eq!(port_of(&procly.procly_in(&main, &["up", "web"])), fallback);
    assert_success(&procly.procly_in(&main, &["down", "web"]));

    // Once the declared port is free, the next start takes it back.
    drop(foreign);
    assert_eq!(port_of(&procly.procly_in(&main, &["up", "web"])), declared);
}

#[test]
fn a_fallback_lease_written_by_0_5_0_keeps_its_port_while_the_declared_port_is_held_across_restarts() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let (declared, fallback) = (free_port(), free_port());
    write(&main, "procly.toml", &web_declaring(declared));
    // As procly 0.5.0 stored a Lease of the Main worktree that fell back from its declared port: no `declared`. Used
    // a moment ago, so that it does not expire.
    let used = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("a clock after 1970")
        .as_millis();
    let document = format!(
        "{{\"version\": 2, \"leases\": [{{\"repository\": {:?}, \"worktree\": {:?}, \"service\": \"web\", \
         \"port_name\": \"http\", \"port\": {fallback}, \"last_used_ms\": {used}}}]}}\n",
        main.join(".git").canonicalize().unwrap(),
        main.canonicalize().unwrap()
    );
    std::fs::write(procly.state_dir().join("leases.json"), document).unwrap();
    let foreign = std::net::TcpListener::bind(("127.0.0.1", declared)).expect("hold the declared port");

    for _ in 0..2 {
        assert_eq!(port_of(&procly.procly_in(&main, &["up", "web"])), fallback);
        assert_success(&procly.procly_in(&main, &["down", "web"]));
    }
    procly.stop_daemon();
    assert_eq!(port_of(&procly.procly_in(&main, &["up", "web"])), fallback);
    assert_success(&procly.procly_in(&main, &["down", "web"]));

    drop(foreign);
    assert_eq!(port_of(&procly.procly_in(&main, &["up", "web"])), declared);
}

#[test]
fn restart_refuses_a_new_declared_port_that_is_held_before_it_stops_the_service() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let (old, new) = (free_port(), free_port());
    write(&main, "procly.toml", &web_declaring(old));
    assert_eq!(port_of(&procly.procly_in(&main, &["up", "web"])), old);
    write(&main, "procly.toml", &web_declaring(new));
    let foreign = std::net::TcpListener::bind(("127.0.0.1", new)).expect("hold the new declared port");

    let refused = procly.procly_in(&main, &["restart", "web"]);

    assert_eq!(refused.status.code(), Some(125), "{}", common::stderr(&refused));
    assert!(
        common::stderr(&refused).contains(&format!("the declared port {new} of Port name http of service web")),
        "{}",
        common::stderr(&refused)
    );
    // Nothing was stopped, the Service still answers on its Lease.
    assert_eq!(common::http_get(old, "/"), "ok\n");
    let status = stdout(&procly.procly_in(&main, &["status"]));
    assert!(status.contains(&format!("service web healthy port {old}")), "{status}");
    drop(foreign);
}

#[test]
fn a_declared_port_left_in_time_wait_is_said_to_free_itself_within_a_minute() {
    use std::io::Read as _;

    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let (old, new) = (free_port(), free_port());
    write(&main, "procly.toml", &web_declaring(old));
    assert_eq!(port_of(&procly.procly_in(&main, &["up", "web"])), old);
    assert_success(&procly.procly_in(&main, &["down", "web"]));
    write(&main, "procly.toml", &web_declaring(new));
    // The side that closes first keeps the port in TIME_WAIT once the listener is gone, as a dev server does after
    // answering a request.
    let listener = std::net::TcpListener::bind(("0.0.0.0", new)).expect("listen on the new declared port");
    let mut client = std::net::TcpStream::connect(("127.0.0.1", new)).expect("connect to it");
    let (served, _) = listener.accept().expect("accept the connection");
    drop(served);
    let mut rest = Vec::new();
    client.read_to_end(&mut rest).expect("read until the server closes");
    drop(client);
    drop(listener);

    let refused = procly.procly_in(&main, &["up", "web"]);

    assert_eq!(refused.status.code(), Some(125), "{}", common::stderr(&refused));
    let message = common::stderr(&refused);
    assert!(message.contains("frees the port by itself within a minute"), "{message}");
}

#[test]
fn a_running_service_keeps_its_lease_and_status_and_doctor_say_it_differs_from_the_declaration() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let (old, new) = (free_port(), free_port());
    write(&main, "procly.toml", &web_declaring(old));
    assert_eq!(port_of(&procly.procly_in(&main, &["up", "web"])), old);
    write(&main, "procly.toml", &web_declaring(new));

    assert_eq!(port_of(&procly.procly_in(&main, &["up", "web"])), old);
    let env = procly.procly_in(&main, &["env", "web"]);
    assert!(stdout(&env).contains(&format!("PORT='{old}'")), "{}", stdout(&env));

    let status = stdout(&procly.procly_in(&main, &["status"]));
    assert!(
        status.contains(&format!(
            "Lease web.http port {old} differs from its declared port {new}"
        )),
        "{status}"
    );
    let doctor = procly.procly_in(&main, &["doctor"]);
    let checks = stdout(&doctor);
    assert!(
        checks.contains(&format!(
            "lease web.http port {old} of {} differs from its declared port {new}",
            main.canonicalize().expect("canonical Main worktree").display()
        )),
        "{checks}"
    );

    assert_success(&procly.procly_in(&main, &["down", "web"]));
    assert_eq!(port_of(&procly.procly_in(&main, &["up", "web"])), new);
    let status = stdout(&procly.procly_in(&main, &["status"]));
    assert!(!status.contains("differs from its declared port"), "{status}");
}
