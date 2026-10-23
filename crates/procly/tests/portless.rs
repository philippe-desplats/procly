//! Portless aliases: the stable name `procly up` gives each Lease it prints as an `http://` URL when `[settings]
//! portless` is on, kept as long as the Lease.
//!
//! Every run has a fake `portless` first in the `PATH`, which logs its arguments, and the directories of any real
//! `portless` taken out of it, so that no test reaches the real Portless.

mod common;

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::Duration;

use common::{Procly, assert_success, free_port, git, git_repository, git_worktree_add, stderr, stdout, test_server};

/// The `[settings]` table turning Portless on.
const ON: &str = "[settings]\nportless = true\n";

/// A machine whose `PATH` holds a fake `portless`, or none.
struct Machine {
    procly: Procly,
    /// The directory first in the `PATH`.
    bin: PathBuf,
}

impl Machine {
    /// A machine whose `portless` logs its arguments and succeeds.
    fn new() -> Self {
        Self::from(Procly::new(), 0, 0, true)
    }

    /// Like [`Machine::new`], with a clock that stands still until [`Procly::advance_clock`] moves it.
    fn with_clock() -> Self {
        Self::from(Procly::with_clock(), 0, 0, true)
    }

    /// A machine without `portless`.
    fn without_portless() -> Self {
        Self::from(Procly::new(), 0, 0, false)
    }

    /// A machine whose `portless alias` exits with `alias` and whose `portless alias --remove` exits with `remove`.
    fn failing(alias: i32, remove: i32) -> Self {
        Self::from(Procly::new(), alias, remove, true)
    }

    fn from(procly: Procly, alias: i32, remove: i32, installed: bool) -> Self {
        procly.narrow_range();
        let bin = procly.scratch().join("bin");
        std::fs::create_dir_all(&bin).expect("create the fake tools directory");
        if installed {
            let script = format!(
                "#!/bin/sh\necho \"$*\" >> '{}'\nif [ \"$2\" = \"--remove\" ]; then exit {remove}; fi\n\
                 if [ {alias} -ne 0 ]; then echo 'the proxy is not running' >&2; fi\nexit {alias}\n",
                bin.join("calls").display()
            );
            let path = bin.join("portless");
            std::fs::write(&path, script).expect("write the fake portless");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("make it executable");
        }
        Self { procly, bin }
    }

    /// The `PATH`: the fake tools first, then the usual one without a directory holding a real `portless`.
    fn path(&self) -> std::ffi::OsString {
        let path = std::env::var_os("PATH").expect("PATH is set");
        let usual = std::env::split_paths(&path).filter(|dir| !dir.join("portless").exists());
        std::env::join_paths(std::iter::once(self.bin.clone()).chain(usual)).expect("join the PATH")
    }

    /// `procly` with `args` in `dir`.
    fn run(&self, dir: &Path, args: &[&str]) -> Output {
        self.procly
            .command()
            .current_dir(dir)
            .env("PATH", self.path())
            .args(args)
            .output()
            .expect("the procly binary runs")
    }

    /// What the fake `portless` was called with, one call per line.
    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.bin.join("calls"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// A Repository declaring `web`, which listens on its `http` Lease, and `settings` before it.
    fn repository(&self, settings: &str) -> PathBuf {
        let main = git_repository(self.procly.scratch(), "app");
        write_config(&main, settings);
        main
    }
}

fn write_config(dir: &Path, settings: &str) {
    std::fs::write(
        dir.join("procly.toml"),
        format!(
            "{settings}[services.web]\nrun = '{} --port {{ports.http}}'\nports = {{ http = {} }}\n",
            test_server().display(),
            free_port()
        ),
    )
    .expect("write the configuration");
}

/// The Lease the health check probes, at the end of the first Lease of the line `procly up` printed.
fn port_of(output: &Output) -> u16 {
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

/// Removes the Worktree `path` of the Repository `main`.
fn remove_worktree(main: &Path, path: &Path) {
    git(main, &["worktree", "remove", "--force", &path.to_string_lossy()]);
}

/// Asserts that nothing but `portless alias` ran, whichever arguments it had.
fn assert_only_aliases(machine: &Machine) {
    for call in machine.calls() {
        assert!(call.starts_with("alias "), "unexpected call of portless: {call}");
    }
}

#[test]
fn nothing_calls_portless_unless_the_settings_turn_it_on() {
    let machine = Machine::new();
    let main = machine.repository("");
    let other = git_worktree_add(&main, "app-login");
    write_config(&other, "[settings]\nportless = false\n");

    assert_success(&machine.run(&main, &["up", "web"]));
    assert_success(&machine.run(&other, &["up", "web"]));
    assert_success(&machine.run(&other, &["down", "web"]));
    assert_success(&machine.run(&other, &["worktree", "release"]));
    remove_worktree(&main, &other);
    assert_success(&machine.run(&main, &["gc", "--apply"]));

    assert_eq!(machine.calls(), Vec::<String>::new());
}

#[test]
fn up_aliases_the_http_lease_of_the_main_worktree_to_the_name_of_its_service() {
    let machine = Machine::new();
    let main = machine.repository(ON);

    let output = machine.run(&main, &["up", "web"]);

    let port = port_of(&output);
    assert_eq!(machine.calls(), [format!("alias web {port}")]);
    assert_eq!(stderr(&output), "", "an alias made says nothing");
}

#[test]
fn up_prefixes_the_name_with_the_branch_outside_the_main_worktree() {
    let machine = Machine::new();
    let main = machine.repository(ON);
    let login = git_worktree_add(&main, "app-login");
    write_config(&login, ON);

    let port = port_of(&machine.run(&login, &["up", "web"]));

    assert_eq!(machine.calls(), [format!("alias app-login.web {port}")]);
}

#[test]
fn a_branch_with_a_slash_becomes_one_dns_label() {
    let machine = Machine::new();
    let main = machine.repository(ON);
    let path = main.parent().expect("a parent").join("app-login");
    git(
        &main,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "feature/E-59",
            &path.to_string_lossy(),
        ],
    );
    write_config(&path, ON);

    let port = port_of(&machine.run(&path, &["up", "web"]));

    assert_eq!(machine.calls(), [format!("alias feature-e-59.web {port}")]);
}

#[test]
fn a_pinned_branch_is_named_after_its_branch_and_keeps_its_name_in_the_worktree_that_checks_it_out() {
    let machine = Machine::new();
    let main = machine.repository(&format!("{ON}[pinned.develop]\n"));
    let path = main.parent().expect("a parent").join("app-dev");
    git(
        &main,
        &["worktree", "add", "--quiet", "-b", "develop", &path.to_string_lossy()],
    );
    write_config(&path, &format!("{ON}[pinned.develop]\n"));

    let port = port_of(&machine.run(&path, &["up", "web"]));

    assert_eq!(machine.calls(), [format!("alias develop.web {port}")]);
}

#[test]
fn a_service_with_several_http_leases_names_each_one_after_its_port_name() {
    let machine = Machine::new();
    let main = git_repository(machine.procly.scratch(), "app");
    let config = format!(
        "{ON}[services.api]\nrun = '{} --port {{ports.web}}'\nports = {{ web = {}, http = {} }}\n\
         [services.api.health]\nport = \"web\"\n",
        test_server().display(),
        free_port(),
        free_port()
    );
    std::fs::write(main.join("procly.toml"), config).expect("write the configuration");
    let login = git_worktree_add(&main, "app-login");
    std::fs::copy(main.join("procly.toml"), login.join("procly.toml")).expect("copy the configuration");

    let in_main = machine.run(&main, &["up", "api", "--json"]);
    assert_success(&in_main);
    let leases = |output: &Output| {
        let document: serde_json::Value = serde_json::from_slice(&output.stdout).expect("a JSON document");
        let ports = document["services"][0]["ports"].clone();
        (ports["http"].as_u64().unwrap(), ports["web"].as_u64().unwrap())
    };
    let (http, web) = leases(&in_main);
    let in_login = machine.run(&login, &["up", "api", "--json"]);
    let (login_http, login_web) = leases(&in_login);

    let mut calls = machine.calls();
    calls.sort();
    let mut expected = vec![
        format!("alias http.api {http}"),
        format!("alias web.api {web}"),
        format!("alias app-login.http.api {login_http}"),
        format!("alias app-login.web.api {login_web}"),
    ];
    expected.sort();
    assert_eq!(calls, expected);
}

#[test]
fn a_lease_that_is_not_an_http_url_gets_no_alias() {
    let machine = Machine::new();
    let main = git_repository(machine.procly.scratch(), "app");
    let config = format!(
        "{ON}[services.mail]\nrun = '{} --port {{ports.smtp}}'\nports = {{ smtp = {} }}\n",
        test_server().display(),
        free_port()
    );
    std::fs::write(main.join("procly.toml"), config).expect("write the configuration");

    assert_success(&machine.run(&main, &["up", "mail"]));

    assert_eq!(machine.calls(), Vec::<String>::new());
}

#[test]
fn up_makes_the_alias_again_on_a_service_that_runs_and_the_down_keeps_it() {
    let machine = Machine::new();
    let main = machine.repository(ON);
    let login = git_worktree_add(&main, "app-login");
    write_config(&login, ON);

    let port = port_of(&machine.run(&login, &["up", "web"]));
    assert_success(&machine.run(&login, &["down", "web"]));

    assert_eq!(machine.calls(), [format!("alias app-login.web {port}")]);
    assert_eq!(port_of(&machine.run(&login, &["up", "web"])), port);
    assert_eq!(machine.calls().len(), 2, "{:?}", machine.calls());
    assert_only_aliases(&machine);
}

#[test]
fn worktree_release_removes_the_alias_of_the_leases_it_releases() {
    let machine = Machine::new();
    let main = machine.repository(ON);
    let login = git_worktree_add(&main, "app-login");
    write_config(&login, ON);
    let port = port_of(&machine.run(&login, &["up", "web"]));

    assert_success(&machine.run(&login, &["worktree", "release"]));

    assert_eq!(
        machine.calls(),
        [
            format!("alias app-login.web {port}"),
            "alias --remove app-login.web".to_owned()
        ]
    );
    // Released once: the alias is no longer recorded.
    assert_success(&machine.run(&login, &["worktree", "release"]));
    assert_eq!(machine.calls().len(), 2);
}

#[test]
fn worktree_release_keeps_the_alias_of_a_pinned_branch() {
    let machine = Machine::new();
    let main = machine.repository(&format!("{ON}[pinned.develop]\n"));
    let path = main.parent().expect("a parent").join("app-dev");
    git(
        &main,
        &["worktree", "add", "--quiet", "-b", "develop", &path.to_string_lossy()],
    );
    write_config(&path, &format!("{ON}[pinned.develop]\n"));
    let port = port_of(&machine.run(&path, &["up", "web"]));

    assert_success(&machine.run(&path, &["worktree", "release"]));

    assert_eq!(machine.calls(), [format!("alias develop.web {port}")]);
}

#[test]
fn an_expired_lease_loses_its_alias() {
    let machine = Machine::with_clock();
    let main = machine.repository(ON);
    let login = git_worktree_add(&main, "app-login");
    write_config(&login, ON);
    let port = port_of(&machine.run(&login, &["up", "web"]));
    assert_success(&machine.run(&login, &["down", "web"]));
    machine.procly.advance_clock(Duration::from_hours(29 * 24));
    assert_success(&machine.run(&main, &["up", "web"]));
    assert_eq!(
        machine.calls().len(),
        2,
        "the Lease has not expired yet: {:?}",
        machine.calls()
    );

    machine.procly.advance_clock(Duration::from_hours(2 * 24));
    assert_success(&machine.run(&main, &["up", "web"]));

    let calls = machine.calls();
    assert!(calls.contains(&format!("alias app-login.web {port}")), "{calls:?}");
    assert!(calls.contains(&"alias --remove app-login.web".to_owned()), "{calls:?}");
    assert_only_aliases(&machine);
}

#[test]
fn gc_apply_removes_the_alias_of_the_orphan_leases_it_deletes() {
    let machine = Machine::new();
    let main = machine.repository(ON);
    let login = git_worktree_add(&main, "app-login");
    write_config(&login, ON);
    assert_success(&machine.run(&login, &["up", "web"]));
    assert_success(&machine.run(&login, &["down", "web"]));
    remove_worktree(&main, &login);

    assert_success(&machine.run(&main, &["gc"]));
    assert_eq!(
        machine.calls().len(),
        1,
        "gc without --apply deletes nothing: {:?}",
        machine.calls()
    );
    assert_success(&machine.run(&main, &["gc", "--apply"]));

    assert_eq!(
        machine.calls().last().map(String::as_str),
        Some("alias --remove app-login.web")
    );
    assert_only_aliases(&machine);
}

#[test]
fn a_missing_portless_is_one_warning_and_up_still_succeeds() {
    let machine = Machine::without_portless();
    let main = git_repository(machine.procly.scratch(), "app");
    let config = format!(
        "{ON}[services.web]\nrun = '{server} --port {{ports.http}}'\nports = {{ http = {} }}\n\
         [services.docs]\nrun = '{server} --port {{ports.http}}'\nports = {{ http = {} }}\n",
        free_port(),
        free_port(),
        server = test_server().display(),
    );
    std::fs::write(main.join("procly.toml"), config).expect("write the configuration");

    let output = machine.run(&main, &["up"]);

    assert_success(&output);
    assert_eq!(stdout(&output).lines().count(), 2, "{}", stdout(&output));
    let warnings: Vec<_> = stderr(&output)
        .lines()
        .filter(|line| line.contains("warning"))
        .map(str::to_owned)
        .collect();
    assert_eq!(
        warnings,
        ["procly: warning: portless is not installed, install it or set portless = false in [settings]"]
    );
}

#[test]
fn a_failing_portless_is_a_warning_telling_why_and_up_still_succeeds() {
    let machine = Machine::failing(3, 0);
    let main = machine.repository(ON);

    let output = machine.run(&main, &["up", "web"]);

    let port = port_of(&output);
    assert_eq!(
        stderr(&output),
        format!("procly: warning: `portless alias web {port}` failed: the proxy is not running\n")
    );
    assert_only_aliases(&machine);
}

#[test]
fn a_failing_removal_leaves_the_alias_recorded_and_the_next_release_tries_again() {
    let machine = Machine::failing(0, 1);
    let main = machine.repository(ON);
    let login = git_worktree_add(&main, "app-login");
    write_config(&login, ON);
    assert_success(&machine.run(&login, &["up", "web"]));

    // The release is not failed by the alias that stays.
    assert_success(&machine.run(&login, &["worktree", "release"]));
    assert_eq!(machine.calls().len(), 2);
    let log = machine.procly.daemon_log();
    assert!(
        log.contains("warning: `portless alias --remove app-login.web` failed"),
        "{log}"
    );

    // Any grant in the Repository removes what no Lease holds.
    assert_success(&machine.run(&main, &["up", "web"]));
    assert!(
        machine
            .calls()
            .iter()
            .filter(|call| call.as_str() == "alias --remove app-login.web")
            .count()
            >= 2
    );
    assert_only_aliases(&machine);
}

#[test]
fn a_stale_alias_is_still_removed_once_portless_is_turned_off() {
    let machine = Machine::new();
    let main = machine.repository(ON);
    let login = git_worktree_add(&main, "app-login");
    write_config(&login, ON);
    assert_success(&machine.run(&login, &["up", "web"]));
    write_config(&login, "");

    assert_success(&machine.run(&login, &["worktree", "release"]));

    assert!(machine.calls().contains(&"alias --remove app-login.web".to_owned()));
}
