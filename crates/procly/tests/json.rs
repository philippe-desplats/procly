mod common;

use std::path::Path;

use common::{
    Procly, assert_success, free_port, git, git_repository, git_worktree_add, json_document, json_schema_version,
    json_succeeded, stderr, stdout, test_server,
};
use serde_json::json;

/// Writes `content` to `name` at the root of the Worktree `dir`.
fn write(dir: &Path, name: &str, content: &str) {
    std::fs::write(dir.join(name), content).expect("write the config file");
}

/// A `procly.toml` declaring `name` as a test server leasing the port it declares, plus `extra` lines of its table.
fn server_service(name: &str, port: u16, extra: &str) -> String {
    format!(
        "[services.{name}]\nrun = '{} --port {{ports.http}}'\nports = {{ http = {port} }}\n{extra}",
        test_server().display()
    )
}

#[test]
fn run_json_carries_the_job_identifier_and_the_outcome() {
    let procly = Procly::new();

    let first = procly.procly(&["run", "--json", "sleep 30"]);
    let second = procly.procly(&["run", "--json", "sleep 30"]);

    let (first, second) = (json_succeeded(&first), json_succeeded(&second));
    assert_eq!(first["schema_version"], json_schema_version(&procly));
    assert_eq!(first["outcome"], json!("started"));
    assert_eq!(second["outcome"], json!("reused"));
    assert_eq!(first["job"], second["job"]);
    let job = first["job"].as_str().expect("the job is a string");
    assert_eq!(procly.run(&["sleep 30"]), job);
}

#[test]
fn wait_json_carries_the_exit_code_of_the_job_and_its_captured_output() {
    let procly = Procly::new();
    let job = procly.run(&["echo hello; echo world; exit 3"]);

    let output = procly.procly(&["wait", "--json", &job]);

    assert_eq!(output.status.code(), Some(3), "stderr: {}", stderr(&output));
    assert_eq!(stderr(&output), "");
    let document = json_document(&output);
    assert_eq!(document["schema_version"], json_schema_version(&procly));
    assert_eq!(document["job"], json!(job));
    assert_eq!(document["exit_code"], json!(3));
    assert_eq!(document["end"], json!({ "kind": "exited", "code": 3 }));
    assert_eq!(document["output"], json!("hello\nworld\n"));
}

#[test]
fn wait_json_reports_a_job_killed_by_a_signal_with_the_exit_code_a_shell_would_give() {
    let procly = Procly::new();
    let job = procly.run(&["kill -TERM $$"]);

    let output = procly.procly(&["wait", "--json", &job]);

    assert_eq!(output.status.code(), Some(143));
    let document = json_document(&output);
    assert_eq!(document["exit_code"], json!(143));
    assert_eq!(document["end"], json!({ "kind": "signaled", "signal": 15 }));
}

#[test]
fn wait_json_keeps_the_timeout_message_on_stderr_and_exit_code_124() {
    let procly = Procly::new();
    let job = procly.run(&["--timeout", "1s", "sleep 60"]);

    let output = procly.procly(&["wait", "--json", &job]);

    assert_eq!(output.status.code(), Some(124));
    assert!(
        stderr(&output).contains("timed out after 1s"),
        "stderr: {}",
        stderr(&output)
    );
    let document = json_document(&output);
    assert_eq!(document["exit_code"], json!(124));
    assert_eq!(document["end"], json!({ "kind": "timed_out", "timeout_ms": 1000 }));
}

#[test]
fn logs_json_carries_the_name_and_the_output() {
    let procly = Procly::new();
    let job = procly.run(&["echo hello"]);
    assert_success(&procly.procly(&["wait", &job]));

    let document = json_succeeded(&procly.procly(&["logs", "--json", &job]));

    assert_eq!(document["schema_version"], json_schema_version(&procly));
    assert_eq!(document["name"], json!(job));
    assert_eq!(document["output"], json!("hello\n"));
}

#[test]
fn up_json_carries_the_outcome_and_the_lease_of_the_service_it_reached() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    write(&main, "procly.toml", &server_service("web", free_port(), ""));

    let started = json_succeeded(&procly.procly_in(&main, &["up", "--json", "web"]));
    let reused = json_succeeded(&procly.procly_in(&main, &["up", "--json", "web"]));

    assert_eq!(started["schema_version"], json_schema_version(&procly));
    assert_eq!(started["result"], json!("healthy"));
    let port = started["services"][0]["port"].as_u64().expect("the Lease of web");
    assert_eq!(
        started["services"],
        json!([{
            "service": "web",
            "outcome": "started",
            "via": null,
            "port": port,
            "url": format!("http://localhost:{port}"),
            "ports": { "http": port },
            "needed_by": null,
        }])
    );
    assert_eq!(reused["services"][0]["outcome"], json!("reused"));
    // The same facts as the text.
    let text = procly.procly_in(&main, &["up", "web"]);
    assert_eq!(
        stdout(&text),
        format!("web reused http://localhost:{port} port {port}\n")
    );
}

#[test]
fn up_json_carries_every_lease_of_a_service_declaring_2_port_names() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let (http, admin) = (free_port(), free_port());
    let config = format!(
        "[services.multi]\nrun = '{} --port {{ports.http}}'\nports = {{ http = {http}, admin = {admin} }}\nhealth = {{ port = 'http' }}\n",
        test_server().display()
    );
    write(&main, "procly.toml", &config);

    let asked = json_succeeded(&procly.procly_in(&main, &["up", "--json", "multi"]));
    let all = json_succeeded(&procly.procly_in(&main, &["up", "--json"]));

    let leases = json_succeeded(&procly.procly_in(&main, &["env", "--json", "multi"]));
    let (http, admin) = (&leases["variables"]["PORT_HTTP"], &leases["variables"]["PORT_ADMIN"]);
    let http = http
        .as_str()
        .and_then(|port| port.parse::<u64>().ok())
        .expect("the http Lease");
    let admin = admin
        .as_str()
        .and_then(|port| port.parse::<u64>().ok())
        .expect("the admin Lease");
    assert_ne!(http, admin);
    for document in [&asked, &all] {
        let service = &document["services"][0];
        assert_eq!(service["ports"], json!({ "admin": admin, "http": http }));
        // The existing keys are unchanged: `port` is still the Lease of the health check.
        assert_eq!(service["port"], json!(http));
        assert_eq!(service["url"], json!(format!("http://localhost:{http}")));
    }
    // The text prints what it always did.
    let text = procly.procly_in(&main, &["up", "multi"]);
    assert_eq!(
        stdout(&text),
        format!("multi reused http://localhost:{http} port {http}\n")
    );
}

#[test]
fn up_json_lists_what_the_service_needs_in_the_order_it_came_up() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let config = [
        server_service("web", free_port(), "needs = ['api']\n"),
        server_service("api", free_port(), ""),
    ]
    .concat();
    write(&main, "procly.toml", &config);

    let document = json_succeeded(&procly.procly_in(&main, &["up", "--json", "web"]));

    let services = document["services"].as_array().expect("a list of Services");
    let reached: Vec<_> = services
        .iter()
        .map(|service| (service["service"].as_str(), service["needed_by"].as_str()))
        .collect();
    assert_eq!(reached, [(Some("api"), Some("web")), (Some("web"), None)]);
}

#[test]
fn up_json_without_a_service_lists_every_service_it_brought_up() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let config = [
        server_service("web", free_port(), "needs = ['api']\n"),
        server_service("api", free_port(), ""),
    ]
    .concat();
    write(&main, "procly.toml", &config);

    let document = json_succeeded(&procly.procly_in(&main, &["up", "--json"]));

    assert_eq!(document["result"], json!("healthy"));
    let names: Vec<_> = document["services"]
        .as_array()
        .expect("a list of Services")
        .iter()
        .map(|service| {
            (
                service["service"].as_str(),
                service["outcome"].as_str(),
                service["needed_by"].as_str(),
            )
        })
        .collect();
    assert_eq!(
        names,
        [
            (Some("api"), Some("started"), None),
            (Some("web"), Some("started"), None)
        ]
    );
}

#[test]
fn up_json_of_a_service_that_ends_before_it_is_healthy_exits_3_and_tells_why_on_stdout_and_stderr() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let config = format!(
        "[services.web]\nrun = 'echo cannot bind >&2; exit 7'\nports = {{ http = {} }}\n",
        free_port()
    );
    write(&main, "procly.toml", &config);

    let output = procly.procly_in(&main, &["up", "--json", "web"]);

    assert_eq!(output.status.code(), Some(3), "stderr: {}", stderr(&output));
    assert!(
        stderr(&output).starts_with("procly: service web exited with code 7 before it became healthy"),
        "{}",
        stderr(&output)
    );
    let document = json_document(&output);
    assert_eq!(document["schema_version"], json_schema_version(&procly));
    assert_eq!(document["result"], json!("unhealthy"));
    assert_eq!(document["service"], json!("web"));
    assert_eq!(document["needed_by"], json!(null));
    assert_eq!(document["end"], json!({ "kind": "exited", "code": 7 }));
    assert_eq!(document["output"], json!("cannot bind\n"));
}

#[test]
fn up_json_of_a_port_mismatch_exits_4_as_the_text_does() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let (port, actual) = (free_port(), free_port());
    let config = format!(
        "[services.web]\nrun = '{} --port {actual}'\nports = {{ http = {port} }}\nhealth = {{ timeout = '1s' }}\n",
        test_server().display()
    );
    write(&main, "procly.toml", &config);

    let output = procly.procly_in(&main, &["up", "--json", "web"]);

    assert_eq!(output.status.code(), Some(4), "stderr: {}", stderr(&output));
    let document = json_document(&output);
    assert_eq!(document["result"], json!("unhealthy"));
    assert_eq!(document["end"]["kind"], json!("port_mismatch"));
    assert_eq!(document["end"]["actual"], json!(actual));
}

#[test]
fn env_json_carries_the_variables_the_text_exports() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    write(&main, "procly.toml", &server_service("web", free_port(), ""));

    let document = json_succeeded(&procly.procly_in(&main, &["env", "--json", "web"]));

    let text = stdout(&procly.procly_in(&main, &["env", "web"]));
    assert_eq!(document["schema_version"], json_schema_version(&procly));
    assert_eq!(document["service"], json!("web"));
    let variables = document["variables"].as_object().expect("variables by name");
    assert_eq!(variables["PROCLY"], json!("1"));
    assert_eq!(variables["PROCLY_SERVICE"], json!("web"));
    let port = variables["PORT"].as_str().expect("the Lease");
    assert!(text.contains(&format!("export PORT='{port}'\n")), "{text}");
    assert_eq!(variables.len(), text.lines().count());
}

#[test]
fn down_json_carries_the_services_it_stopped() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    write(&main, "procly.toml", &server_service("web", free_port(), ""));
    assert_success(&procly.procly_in(&main, &["up", "web"]));

    let stopped = json_succeeded(&procly.procly_in(&main, &["down", "--json"]));
    let none = json_succeeded(&procly.procly_in(&main, &["down", "--json", "web"]));

    assert_eq!(stopped["schema_version"], json_schema_version(&procly));
    assert_eq!(stopped["service"], json!(null));
    assert_eq!(stopped["stopped"], json!(["web"]));
    assert_eq!(stopped["released"], json!([]));
    assert_eq!(none["service"], json!("web"));
    assert_eq!(none["stopped"], json!([]));
}

/// A Repository whose Main worktree holds a `package.json` with a `dev` script, so that `init` has a proposal.
fn repository_with_a_dev_script(procly: &Procly) -> std::path::PathBuf {
    let main = git_repository(procly.scratch(), "app");
    write(
        &main,
        "package.json",
        "{\"name\": \"app\", \"scripts\": {\"dev\": \"vite\"}}\n",
    );
    main
}

#[test]
fn init_json_carries_the_proposal_and_whether_it_was_written() {
    let procly = Procly::new();
    let main = repository_with_a_dev_script(&procly);

    let written = json_succeeded(&procly.procly_in(&main, &["init", "--json"]));
    let left = json_succeeded(&procly.procly_in(&main, &["init", "--json"]));

    assert_eq!(written["schema_version"], json_schema_version(&procly));
    assert_eq!(written["file"], json!(main.join("procly.toml")));
    assert_eq!(written["written"], json!(true));
    let on_disk = std::fs::read_to_string(main.join("procly.toml")).expect("the proposal is written");
    assert_eq!(written["toml"], json!(on_disk));
    assert_eq!(left["written"], json!(false));
    assert_eq!(left["toml"], written["toml"]);
}

/// A Repository whose `procly.toml` holds `config`, with a second Worktree checked out from it.
fn repository_with_worktree(procly: &Procly, config: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let main = git_repository(procly.scratch(), "app");
    write(&main, "procly.toml", config);
    git(&main, &["add", "procly.toml"]);
    git(
        &main,
        &["commit", "--quiet", "--message", "declare the worktree section"],
    );
    let worktree = git_worktree_add(&main, "feature");
    (main, worktree)
}

#[test]
fn worktree_init_json_carries_what_was_copied_and_what_ran() {
    let procly = Procly::new();
    let config = "[worktree]\ncopy = ['.env', 'missing.txt']\nsetup = ['echo ready >> setup.log']\n";
    let (main, worktree) = repository_with_worktree(&procly, config);
    write(&main, ".env", "SECRET=1\n");

    let document = json_succeeded(&procly.procly_in(&worktree, &["worktree", "init", "--json"]));
    let nothing = json_succeeded(&procly.procly_in(&worktree, &["worktree", "init", "--json"]));

    assert_eq!(document["schema_version"], json_schema_version(&procly));
    assert_eq!(document["result"], json!("ready"));
    assert_eq!(document["copied"], json!([".env"]));
    assert_eq!(document["missing"], json!(["missing.txt"]));
    assert_eq!(document["ran"], json!(["echo ready >> setup.log"]));
    assert_eq!(nothing["ran"], json!([]));
    assert_eq!(nothing["copied"], json!([]));
}

#[test]
fn worktree_init_json_of_a_failing_setup_command_exits_with_its_code_and_tells_why() {
    let procly = Procly::new();
    let config = "[worktree]\nsetup = ['echo first >> log', 'echo boom; exit 7', 'echo third >> log']\n";
    let (_, worktree) = repository_with_worktree(&procly, config);

    let output = procly.procly_in(&worktree, &["worktree", "init", "--json"]);

    assert_eq!(output.status.code(), Some(7), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("setup command `echo boom; exit 7` exited with code 7"),
        "{}",
        stderr(&output)
    );
    let document = json_document(&output);
    assert_eq!(document["result"], json!("setup_failed"));
    assert_eq!(document["ran"], json!(["echo first >> log"]));
    assert_eq!(document["command"], json!("echo boom; exit 7"));
    assert_eq!(document["end"], json!({ "kind": "exited", "code": 7 }));
    assert_eq!(document["output"], json!("boom\n"));
}

#[test]
fn agent_install_json_carries_what_became_of_the_rules_block_and_the_guard() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    write(&main, "AGENTS.md", "# App\n");

    let written = json_succeeded(&procly.procly_in(&main, &["agent", "install", "--json"]));
    let unchanged = json_succeeded(&procly.procly_in(&main, &["agent", "install", "--json"]));
    let edited_rules = std::fs::read_to_string(main.join("AGENTS.md"))
        .expect("the rules are written")
        .replace("Never start", "Please avoid starting");
    write(&main, "AGENTS.md", &edited_rules);
    let edited = json_succeeded(&procly.procly_in(&main, &["agent", "install", "--json"]));

    assert_eq!(written["schema_version"], json_schema_version(&procly));
    assert_eq!(
        written["rules"],
        json!({ "file": main.join("AGENTS.md"), "outcome": "written" })
    );
    assert_eq!(
        written["guard"],
        json!({ "file": main.join(".claude/settings.json"), "outcome": "written" })
    );
    assert_eq!(written["off"], json!(false));
    assert_eq!(unchanged["rules"]["outcome"], json!("unchanged"));
    assert_eq!(unchanged["guard"]["outcome"], json!("unchanged"));
    assert_eq!(edited["rules"]["outcome"], json!("edited"));
}

#[test]
fn off_and_on_json_carry_the_new_state_and_the_rules_blocks() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    write(&main, "AGENTS.md", "# App\n");
    assert_success(&procly.procly_in(&main, &["agent", "install"]));

    let off = json_succeeded(&procly.procly_in(&main, &["off", "--json"]));
    let on = json_succeeded(&procly.procly_in(&main, &["on", "--json"]));

    assert_eq!(off["schema_version"], json_schema_version(&procly));
    assert_eq!(off["off"], json!(true));
    assert_eq!(off["rules"][0]["file"], json!(main.join("AGENTS.md")));
    assert_eq!(off["rules"][0]["outcome"], json!("written"));
    assert_eq!(on["off"], json!(false));
    assert_eq!(on["rules"][0]["file"], json!(main.join("AGENTS.md")));
}
