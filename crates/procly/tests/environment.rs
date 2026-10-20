//! Derived variables, the Env file and the git exclusion of the files Procly generates (ADR 0007, ADR 0008).

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use common::{
    Procly, assert_success, free_port, git_output, git_repository, git_worktree_add, stderr, stdout, test_server,
};

const EXCLUSION_BLOCK: &str = "# >>> procly managed block: exclusions >>>\n/.env.procly\n/procly.local.toml\n# <<< procly managed block: exclusions <<<\n";

fn write(dir: &Path, name: &str, content: &str) {
    fs::write(dir.join(name), content).expect("write the file");
}

/// A `procly.toml` with `api`, and `web` which dumps its environment to `dump` before it listens. `extra` lines go
/// under `[services.web.env]`.
fn api_and_web(dump: &Path, extra: &str) -> String {
    format!(
        "[services.api]\nrun = '{server} --port {{ports.http}}'\nports = {{ http = {api} }}\n\
         [services.web]\nrun = 'env > {dump}; exec {server} --port {{ports.http}}'\nports = {{ http = {web} }}\n\
         [services.web.env]\nAPI_URL = 'http://localhost:{{services.api.ports.http}}'\nSESSION = 'web_{{worktree.slug}}'\n{extra}",
        server = test_server().display(),
        dump = dump.display(),
        api = free_port(),
        web = free_port(),
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

/// The variables a program dumped with `env`, by name.
fn parse_env(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|line| line.split_once('='))
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect()
}

fn dumped(dump: &Path) -> BTreeMap<String, String> {
    common::eventually("the service to dump its environment", || dump.exists());
    parse_env(&fs::read_to_string(dump).expect("read the dump"))
}

/// A Repository with a second Worktree named `app-other`, whose slug is `app_other`. Returns both.
fn repository(procly: &Procly) -> (PathBuf, PathBuf) {
    let main = git_repository(procly.scratch(), "app");
    let other = git_worktree_add(&main, "app-other");
    (main, other)
}

#[test]
fn a_template_over_another_services_lease_and_the_worktree_slug_renders_in_the_process_environment() {
    let procly = Procly::new();
    procly.narrow_range();
    let (_, other) = repository(&procly);
    let dump = procly.scratch().join("web-env");
    write(&other, "procly.toml", &api_and_web(&dump, ""));
    let api = port_of(&procly.procly_in(&other, &["up", "api"]));

    // The caller's own value never wins over a Derived variable, nor does the repository's `.env`.
    let output = procly
        .command()
        .current_dir(&other)
        .env("API_URL", "from_the_caller")
        .args(["up", "web"])
        .output()
        .expect("procly runs");
    assert_success(&output);

    let env = dumped(&dump);
    assert_eq!(env["API_URL"], format!("http://localhost:{api}"));
    assert_eq!(env["SESSION"], "web_app_other");
    assert_eq!(env["PROCLY"], "1");
    assert_eq!(env["PROCLY_SERVICE"], "web");
}

#[test]
fn a_service_started_before_the_one_it_refers_to_gets_that_services_lease_all_the_same() {
    let procly = Procly::new();
    procly.narrow_range();
    let (_, other) = repository(&procly);
    let dump = procly.scratch().join("web-env");
    write(&other, "procly.toml", &api_and_web(&dump, ""));

    assert_success(&procly.procly_in(&other, &["up", "web"]));
    let api = port_of(&procly.procly_in(&other, &["up", "api"]));

    assert_eq!(dumped(&dump)["API_URL"], format!("http://localhost:{api}"));
}

#[test]
fn the_repositorys_dotenv_is_byte_identical_after_up() {
    let procly = Procly::new();
    let (main, _) = repository(&procly);
    let dump = procly.scratch().join("web-env");
    write(&main, "procly.toml", &api_and_web(&dump, ""));
    // No trailing newline, and a PORT line, which a tool rewriting the file would touch.
    let dotenv = "API_URL=http://localhost:3000\nPORT=3000\nSESSION=mine";
    write(&main, ".env", dotenv);

    assert_success(&procly.procly_in(&main, &["up", "web"]));
    assert_success(&procly.procly_in(&main, &["up", "web"]));

    assert_eq!(fs::read(main.join(".env")).unwrap(), dotenv.as_bytes());
}

#[test]
fn the_env_file_holds_the_derived_variables_and_is_excluded_from_git() {
    let procly = Procly::new();
    procly.narrow_range();
    let (main, other) = repository(&procly);
    let dump = procly.scratch().join("web-env");
    write(&other, "procly.toml", &api_and_web(&dump, ""));
    write(&other, "procly.local.toml", "");
    let exclude = main.join(".git/info/exclude");
    let before = fs::read_to_string(&exclude).expect("git creates the exclude file");
    let api = port_of(&procly.procly_in(&other, &["up", "api"]));

    assert_success(&procly.procly_in(&other, &["up", "web"]));

    let env_file = fs::read_to_string(other.join(".env.procly")).expect("the Env file exists");
    assert!(
        env_file
            .lines()
            .any(|line| line == format!("API_URL=http://localhost:{api}")),
        "{env_file}"
    );
    assert!(
        env_file.lines().any(|line| line == "SESSION=web_app_other"),
        "{env_file}"
    );
    assert!(!env_file.contains("PORT") && !env_file.contains("PROCLY"), "{env_file}");
    // What git was told, in the worktree that holds the files and in the other one.
    for worktree in [&other, &main] {
        for file in [".env.procly", "procly.local.toml"] {
            git_output(worktree, &["check-ignore", "--quiet", file]);
        }
    }
    let status = git_output(&other, &["status", "--porcelain", "--untracked-files=all"]);
    assert_eq!(status.lines().collect::<Vec<_>>(), ["?? procly.toml"], "{status}");
    // The default content of the file is kept as it was, the block comes after it.
    assert_eq!(
        fs::read_to_string(&exclude).unwrap(),
        format!("{before}{EXCLUSION_BLOCK}")
    );
}

#[test]
fn the_exclusion_is_written_once_whatever_the_number_of_ups() {
    let procly = Procly::new();
    procly.narrow_range();
    let (main, other) = repository(&procly);
    let dump = procly.scratch().join("web-env");
    write(&other, "procly.toml", &api_and_web(&dump, ""));
    write(&main, "procly.toml", &api_and_web(&dump, ""));
    let exclude = main.join(".git/info/exclude");

    assert_success(&procly.procly_in(&other, &["up", "web"]));
    let first = fs::read(&exclude).unwrap();
    assert_success(&procly.procly_in(&other, &["up", "web"]));
    assert_success(&procly.procly_in(&other, &["up", "api"]));
    assert_success(&procly.procly_in(&main, &["up", "api"]));
    procly.stop_daemon();
    assert_success(&procly.procly_in(&other, &["up", "api"]));

    assert_eq!(fs::read(&exclude).unwrap(), first);
    let document = fs::metadata(procly.state_dir().join("managed-blocks.json")).expect("the fingerprint is stored");
    assert_eq!(
        std::os::unix::fs::PermissionsExt::mode(&document.permissions()) & 0o777,
        0o600
    );
}

#[test]
fn a_block_the_user_edited_is_left_as_it_is() {
    let procly = Procly::new();
    let (main, _) = repository(&procly);
    let dump = procly.scratch().join("web-env");
    write(&main, "procly.toml", &api_and_web(&dump, ""));
    assert_success(&procly.procly_in(&main, &["up", "api"]));
    let exclude = main.join(".git/info/exclude");
    let edited = fs::read_to_string(&exclude)
        .unwrap()
        .replace("/procly.local.toml\n", "/mine\n");
    fs::write(&exclude, &edited).unwrap();

    assert_success(&procly.procly_in(&main, &["up", "web"]));

    assert_eq!(fs::read_to_string(&exclude).unwrap(), edited);
}

#[test]
fn gitignore_on_request_writes_the_block_there_and_a_later_up_keeps_using_that_file() {
    let procly = Procly::new();
    let (main, other) = repository(&procly);
    let dump = procly.scratch().join("web-env");
    write(&other, "procly.toml", &api_and_web(&dump, ""));
    write(&other, ".gitignore", "target\n# no trailing newline");
    let exclude = main.join(".git/info/exclude");
    let before = fs::read_to_string(&exclude).unwrap();

    assert_success(&procly.procly_in(&other, &["up", "--gitignore", "api"]));

    let gitignore = fs::read_to_string(other.join(".gitignore")).unwrap();
    assert_eq!(gitignore, format!("target\n# no trailing newline\n{EXCLUSION_BLOCK}"));
    assert_eq!(fs::read_to_string(&exclude).unwrap(), before);
    // Without the flag, the file that holds the block is the one still used, so the lines never end up in both.
    assert_success(&procly.procly_in(&other, &["down"]));
    assert_success(&procly.procly_in(&other, &["up", "web"]));
    assert_eq!(fs::read_to_string(other.join(".gitignore")).unwrap(), gitignore);
    assert_eq!(fs::read_to_string(&exclude).unwrap(), before);
    git_output(&other, &["check-ignore", "--quiet", ".env.procly"]);
}

#[test]
fn procly_env_prints_the_variables_the_service_received_as_shell_exports() {
    let procly = Procly::new();
    procly.narrow_range();
    let (_, other) = repository(&procly);
    let dump = procly.scratch().join("web-env");
    write(
        &other,
        "procly.toml",
        &api_and_web(&dump, "GREETING = \"it's $HOME `x` \\\"quoted\\\"\"\n"),
    );
    assert_success(&procly.procly_in(&other, &["up", "web"]));
    let injected = dumped(&dump);

    let exports = procly.procly_in(&other, &["env", "web"]);
    assert_success(&exports);
    // A shell with nothing but those exports, as an agent running a one-off command would have.
    let shell = std::process::Command::new("env")
        .args(["-i", "/bin/sh", "-c", "eval \"$1\"; env", "sh", &stdout(&exports)])
        .output()
        .expect("sh runs");
    assert_success(&shell);
    let mut evaluated = parse_env(&stdout(&shell));
    for noise in ["PWD", "SHLVL", "_", "OLDPWD"] {
        evaluated.remove(noise);
    }

    let names = [
        "API_URL",
        "SESSION",
        "GREETING",
        "PORT",
        "PORT_HTTP",
        "PROCLY",
        "PROCLY_SERVICE",
    ];
    assert_eq!(evaluated.keys().map(String::as_str).collect::<Vec<_>>(), {
        let mut sorted = names.to_vec();
        sorted.sort_unstable();
        sorted
    });
    for name in names {
        assert_eq!(evaluated[name], injected[name], "{name}");
    }
    assert_eq!(injected["GREETING"], "it's $HOME `x` \"quoted\"");
}

#[test]
fn procly_env_fails_for_an_undeclared_service() {
    let procly = Procly::new();
    let (main, _) = repository(&procly);
    write(&main, "procly.toml", &api_and_web(&procly.scratch().join("unused"), ""));

    let output = procly.procly_in(&main, &["env", "nope"]);

    assert_eq!(output.status.code(), Some(125));
    assert!(
        stderr(&output).contains("no service nope is declared"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn an_invalid_derived_variable_fails_up_with_125_before_anything_starts() {
    let procly = Procly::new();
    let (main, _) = repository(&procly);
    let dump = procly.scratch().join("web-env");
    for (extra, why) in [
        ("TYPO = '{worktree.slg}'\n", "{worktree.slg}"),
        (
            "OTHER = '{services.db.ports.http}'\n",
            "service db, which is not declared",
        ),
        ("PORT = '1'\n", "Procly injects it itself"),
        ("'A B' = '1'\n", "cannot name a Derived variable"),
    ] {
        write(&main, "procly.toml", &api_and_web(&dump, extra));

        let output = procly.procly_in(&main, &["up", "web"]);

        assert_eq!(output.status.code(), Some(125), "{extra}");
        assert!(stderr(&output).contains(why), "{extra}: {}", stderr(&output));
        assert!(!dump.exists());
        assert!(!main.join(".env.procly").exists());
    }
}
