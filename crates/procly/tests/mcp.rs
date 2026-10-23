//! `procly mcp`: the verbs of the Daemon as MCP tools over stdio (ADR 0009). Every test speaks real JSON-RPC to the
//! server and compares what a tool returns with what the CLI prints for the same request.

mod common;

use std::collections::BTreeSet;
use std::fs;
use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use common::{
    DEADLINE, Procly, assert_success, free_port, git, git_repository, git_worktree_add, stderr, stdout, test_server,
};
use serde_json::{Value, json};

/// How long a test waits for the server to answer a request, longer than the Jobs it waits for.
const ANSWER_DEADLINE: Duration = Duration::from_secs(30);

/// A `procly mcp` process and the JSON-RPC conversation with it.
struct McpServer {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    next_id: u64,
}

impl McpServer {
    /// Starts `procly mcp` in `dir` and completes the MCP handshake.
    fn start_in(procly: &Procly, dir: &Path) -> Self {
        let mut command = procly.command();
        command.current_dir(dir);
        Self::start_command(command)
    }

    /// Starts `procly mcp` as `command` runs it, with the working directory and environment it carries.
    fn start_command(mut command: Command) -> Self {
        let mut child = command
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("the procly binary runs");
        let stdin = child.stdin.take().expect("stdin is piped");
        let stdout = child.stdout.take().expect("stdout is piped");
        let (sender, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if sender.send(line).is_err() {
                    return;
                }
            }
        });
        let mut server = Self {
            child,
            stdin: Some(stdin),
            lines,
            next_id: 1,
        };
        let initialized = server.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "procly-test", "version": "0" }
            }),
        );
        assert!(initialized.get("result").is_some(), "initialize failed: {initialized}");
        server.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        server
    }

    fn start(procly: &Procly) -> Self {
        Self::start_in(procly, procly.scratch())
    }

    fn send(&mut self, message: &Value) {
        let stdin = self.stdin.as_mut().expect("the input of the server is open");
        writeln!(stdin, "{message}").expect("write to the server");
        stdin.flush().expect("flush to the server");
    }

    /// Sends a request and returns the whole JSON-RPC response, skipping the notifications before it.
    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let mut message = json!({ "jsonrpc": "2.0", "id": id, "method": method });
        message["params"] = params;
        self.send(&message);
        loop {
            let line = self
                .lines
                .recv_timeout(ANSWER_DEADLINE)
                .unwrap_or_else(|error| panic!("no answer to {method} ({error})"));
            let message: Value = serde_json::from_str(&line).unwrap_or_else(|error| {
                panic!("the server wrote something that is not JSON on stdout ({error}): {line:?}")
            });
            if message["id"] == json!(id) {
                return message;
            }
        }
    }

    /// Calls the tool `name` and returns its `result`.
    fn call(&mut self, name: &str, arguments: Value) -> Value {
        let mut params = json!({ "name": name });
        params["arguments"] = arguments;
        let response = self.request("tools/call", params);
        assert!(response.get("error").is_none(), "{name} was refused: {response}");
        response["result"].clone()
    }

    /// The text of the tool `name` when it succeeds, the one block of text it returns.
    fn text(&mut self, name: &str, arguments: Value) -> String {
        let result = self.call(name, arguments);
        assert_ne!(result["isError"], json!(true), "{name} failed: {result}");
        text_of(&result)
    }
}

impl Drop for McpServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The text of the one content block of a tool result.
fn text_of(result: &Value) -> String {
    let content = result["content"].as_array().expect("the result has content");
    assert!(!content.is_empty(), "no content in {result}");
    assert_eq!(content[0]["type"], json!("text"), "{result}");
    content[0]["text"].as_str().expect("the block holds text").to_owned()
}

/// The document a verb printed, which a tool returns as its text, or in `structuredContent` when the verb exited with
/// a code, as `doctor` does when it found a problem.
fn document_of(result: &Value) -> String {
    if result["isError"] == json!(true) {
        result["structuredContent"]["stdout"]
            .as_str()
            .expect("the document of the verb")
            .to_owned()
    } else {
        text_of(result)
    }
}

/// A `procly.toml` declaring `name` as a test server leasing the port it declares.
fn server_service(name: &str) -> String {
    format!(
        "[services.{name}]\nrun = '{} --port {{ports.http}}'\nports = {{ http = {} }}\n",
        test_server().display(),
        free_port()
    )
}

/// A Repository whose Main worktree holds `config` as `procly.toml`, with a second Worktree checked out from it.
fn repository_with_worktree(procly: &Procly, config: &str) -> (PathBuf, PathBuf) {
    let main = git_repository(procly.scratch(), "app");
    fs::write(main.join("procly.toml"), config).expect("write procly.toml");
    git(&main, &["add", "procly.toml"]);
    git(&main, &["commit", "--quiet", "--message", "declare the services"]);
    let worktree = git_worktree_add(&main, "feature");
    (main, worktree)
}

#[test]
fn the_server_exposes_the_verbs_of_the_daemon_as_tools_and_nothing_else() {
    let procly = Procly::new();
    let mut server = McpServer::start(&procly);

    let response = server.request("tools/list", json!({}));

    let tools = response["result"]["tools"].as_array().expect("a list of tools");
    let names: BTreeSet<&str> = tools
        .iter()
        .map(|tool| tool["name"].as_str().expect("a name"))
        .collect();
    let expected = [
        "run",
        "wait",
        "logs",
        "up",
        "down",
        "env",
        "status",
        "worktree_init",
        "worktree_release",
        "gc",
        "doctor",
    ];
    assert_eq!(names, expected.into_iter().collect::<BTreeSet<_>>());
    for tool in tools {
        assert_eq!(tool["inputSchema"]["type"], json!("object"), "{tool}");
        assert!(
            tool["inputSchema"]["properties"].get("cwd").is_some(),
            "{} takes no working directory",
            tool["name"]
        );
    }
}

#[test]
fn the_argument_schemas_mirror_the_arguments_of_the_cli() {
    let procly = Procly::new();
    let mut server = McpServer::start(&procly);

    let response = server.request("tools/list", json!({}));

    let tools = response["result"]["tools"].as_array().expect("a list of tools");
    let properties = |tool: &str, required: bool| -> BTreeSet<String> {
        let schema = &tools
            .iter()
            .find(|candidate| candidate["name"] == json!(tool))
            .expect(tool)["inputSchema"];
        let key = if required { "required" } else { "properties" };
        match &schema[key] {
            Value::Object(map) => map.keys().cloned().collect(),
            Value::Array(items) => items
                .iter()
                .map(|item| item.as_str().expect("a name").to_owned())
                .collect(),
            _ => BTreeSet::new(),
        }
    };
    let set = |names: &[&str]| names.iter().map(|name| (*name).to_owned()).collect::<BTreeSet<_>>();
    assert_eq!(properties("run", false), set(&["command", "timeout", "cwd"]));
    assert_eq!(properties("run", true), set(&["command"]));
    assert_eq!(properties("wait", true), set(&["job"]));
    assert_eq!(properties("logs", true), set(&["job_or_service"]));
    assert_eq!(properties("up", false), set(&["service", "gitignore", "cwd"]));
    assert_eq!(properties("up", true), set(&[]));
    assert_eq!(properties("down", false), set(&["service", "cwd"]));
    assert_eq!(properties("env", true), set(&["service"]));
    assert_eq!(properties("status", false), set(&["all", "cwd"]));
    assert_eq!(properties("worktree_init", false), set(&["path", "cwd"]));
    assert_eq!(properties("worktree_release", false), set(&["path", "cwd"]));
    assert_eq!(properties("gc", false), set(&["apply", "cwd"]));
    assert_eq!(properties("gc", true), set(&[]));
    assert_eq!(properties("doctor", false), set(&["cwd"]));
}

#[test]
fn a_verb_that_is_not_exposed_is_refused() {
    let procly = Procly::new();
    let mut server = McpServer::start(&procly);

    for name in ["init", "unit", "guard", "agent", "off", "on", "eject", "worktree_hooks"] {
        let response = server.request("tools/call", json!({ "name": name, "arguments": {} }));
        let refused = response.get("error").is_some() || response["result"]["isError"] == json!(true);
        assert!(refused, "{name} was served: {response}");
    }
}

#[test]
fn status_returns_the_status_json_of_the_cli() {
    let procly = Procly::new();
    let mut server = McpServer::start(&procly);

    let tool = server.text("status", json!({}));

    let cli = procly.procly(&["status", "--json"]);
    assert_success(&cli);
    assert_eq!(tool, stdout(&cli));
}

#[test]
fn procly_mcp_starts_the_daemon_on_demand() {
    let procly = Procly::new();
    let mut server = McpServer::start(&procly);
    assert!(
        !procly.state_dir().join("daemon.pid").exists(),
        "the server started the Daemon before any tool call"
    );

    server.text("status", json!({}));

    assert!(
        procly.state_dir().join("daemon.pid").exists(),
        "the tool call did not start the Daemon"
    );
}

#[test]
fn run_returns_the_run_json_of_the_cli() {
    let procly = Procly::new();
    let mut server = McpServer::start(&procly);

    let started = procly.procly(&["run", "--json", "sleep 30"]);
    assert_success(&started);
    let reused = server.text("run", json!({ "command": "sleep 30" }));

    let cli = procly.procly(&["run", "--json", "sleep 30"]);
    assert_eq!(reused, stdout(&cli));
    assert!(reused.contains("\"outcome\":\"reused\""), "{reused}");
}

#[test]
fn an_argument_that_starts_with_a_dash_is_a_value_not_a_flag() {
    let procly = Procly::new();
    let mut server = McpServer::start(&procly);

    let started = server.text("run", json!({ "command": "-version" }));
    let unknown = server.call("env", json!({ "service": "-web" }));

    assert!(started.contains("\"started\""), "{started}");
    // A usage error would exit with 2, a Service that is not declared with 125.
    assert_eq!(unknown["structuredContent"]["exit_code"], json!(125), "{unknown}");
}

#[test]
fn run_takes_the_timeout_of_the_cli_and_wait_reports_the_timeout_as_a_tool_error() {
    let procly = Procly::new();
    let mut server = McpServer::start(&procly);
    let job = serde_json::from_str::<Value>(&server.text("run", json!({ "command": "sleep 60", "timeout": "1s" })))
        .expect("run prints a document")["job"]
        .as_str()
        .expect("a job")
        .to_owned();

    let result = server.call("wait", json!({ "job": job }));

    let cli = procly.procly(&["wait", "--json", &job]);
    assert_eq!(cli.status.code(), Some(124));
    assert_eq!(result["isError"], json!(true), "{result}");
    assert_eq!(result["structuredContent"]["exit_code"], json!(124), "{result}");
    assert_eq!(result["structuredContent"]["stderr"], json!(stderr(&cli)), "{result}");
    assert!(text_of(&result).contains("124"), "{result}");
    assert!(text_of(&result).contains("timed out after 1s"), "{result}");
}

#[test]
fn wait_and_logs_return_their_result_in_one_response() {
    let procly = Procly::new();
    let mut server = McpServer::start(&procly);
    let job = procly.run(&["echo hello; echo world"]);

    let waited = server.text("wait", json!({ "job": job }));
    let logged = server.text("logs", json!({ "job_or_service": job }));

    let cli_wait = procly.procly(&["wait", "--json", &job]);
    let cli_logs = procly.procly(&["logs", "--json", &job]);
    assert_eq!(waited, stdout(&cli_wait));
    assert_eq!(logged, stdout(&cli_logs));
    assert!(waited.contains("hello\\nworld\\n"), "{waited}");
}

#[test]
fn a_job_that_exits_with_a_code_is_a_tool_error_carrying_that_code_and_the_document() {
    let procly = Procly::new();
    let mut server = McpServer::start(&procly);
    let job = procly.run(&["echo before; exit 3"]);

    let result = server.call("wait", json!({ "job": job }));

    let cli = procly.procly(&["wait", "--json", &job]);
    assert_eq!(result["isError"], json!(true), "{result}");
    assert_eq!(result["structuredContent"]["exit_code"], json!(3), "{result}");
    assert_eq!(result["structuredContent"]["stdout"], json!(stdout(&cli)), "{result}");
}

#[test]
fn a_failing_verb_returns_a_tool_error_with_its_exit_code_and_its_stderr() {
    let procly = Procly::new();
    let mut server = McpServer::start(&procly);

    let result = server.call("logs", json!({ "job_or_service": "nothing-by-that-name" }));

    let cli = procly.procly(&["logs", "--json", "nothing-by-that-name"]);
    assert_eq!(cli.status.code(), Some(125));
    assert_eq!(result["isError"], json!(true), "{result}");
    assert_eq!(result["structuredContent"]["exit_code"], json!(125), "{result}");
    assert_eq!(result["structuredContent"]["stderr"], json!(stderr(&cli)), "{result}");
    assert!(text_of(&result).contains(stderr(&cli).trim()), "{result}");
}

#[test]
fn env_up_and_down_return_the_json_of_the_cli() {
    let procly = Procly::new();
    let (main, _) = repository_with_worktree(&procly, &server_service("web"));
    let mut server = McpServer::start_in(&procly, &main);
    let cli = |args: &[&str]| {
        let output = procly.procly_in(&main, args);
        assert_success(&output);
        stdout(&output)
    };

    assert_eq!(
        server.text("env", json!({ "service": "web" })),
        cli(&["env", "--json", "web"])
    );

    let started = server.text("up", json!({ "service": "web" }));
    assert!(started.contains("\"started\""), "{started}");
    let reused = server.text("up", json!({ "service": "web" }));
    assert!(reused.contains("\"reused\""), "{reused}");
    assert_eq!(reused, cli(&["up", "--json", "web"]));

    // Stopped by the tool, then started and stopped by the CLI: the 2 documents tell the same stop.
    let stopped = server.text("down", json!({ "service": "web" }));
    cli(&["up", "--json", "web"]);
    assert_eq!(stopped, cli(&["down", "--json", "web"]));
}

#[test]
fn a_tool_runs_in_the_working_directory_it_is_given_or_in_the_one_the_server_started_in() {
    let procly = Procly::new();
    let (main, worktree) = repository_with_worktree(&procly, &server_service("web"));
    let mut server = McpServer::start_in(&procly, &main);

    let default = server.text("env", json!({ "service": "web" }));
    let given = server.text("env", json!({ "service": "web", "cwd": worktree }));

    let cli_main = procly.procly_in(&main, &["env", "--json", "web"]);
    let cli_worktree = procly.procly_in(&worktree, &["env", "--json", "web"]);
    assert_eq!(default, stdout(&cli_main));
    assert_eq!(given, stdout(&cli_worktree));
    assert_ne!(default, given, "the 2 Worktrees must not answer alike");
}

#[test]
fn worktree_init_returns_the_json_of_the_cli() {
    let procly = Procly::new();
    let (main, worktree) = repository_with_worktree(&procly, "[worktree]\ncopy = ['.env']\n");
    fs::write(main.join(".env"), "SECRET=1\n").expect("write the env file");
    let mut server = McpServer::start_in(&procly, &main);

    let first = server.text("worktree_init", json!({ "cwd": worktree }));
    let again = server.text("worktree_init", json!({ "path": worktree }));

    assert!(first.contains(".env"), "{first}");
    assert_eq!(fs::read_to_string(worktree.join(".env")).unwrap(), "SECRET=1\n");
    let cli = procly.procly_in(&worktree, &["worktree", "init", "--json"]);
    assert_success(&cli);
    assert_eq!(again, stdout(&cli));
}

/// A Repository with a Worktree `app-feature` whose Service `web` runs, and the Worktree itself.
fn running_worktree(procly: &Procly) -> (PathBuf, PathBuf) {
    procly.narrow_range();
    let (main, worktree) = repository_with_worktree(procly, &server_service("web"));
    assert_success(&procly.procly_in(&worktree, &["up", "web"]));
    (main, worktree)
}

#[test]
fn worktree_release_returns_the_json_of_the_cli() {
    let procly = Procly::new();
    let (main, worktree) = running_worktree(&procly);
    let mut server = McpServer::start_in(&procly, &main);

    // Released by the tool, then started and released by the CLI: the 2 documents tell the same release.
    let released = server.text("worktree_release", json!({ "cwd": worktree }));
    assert!(released.contains("web"), "{released}");
    assert_success(&procly.procly_in(&worktree, &["up", "web"]));
    let cli = procly.procly_in(&worktree, &["worktree", "release", "--json"]);
    assert_success(&cli);
    assert_eq!(released, stdout(&cli));

    // Nothing is left to release, from the Worktree the server was not started in and by its path.
    let again = server.text("worktree_release", json!({ "path": worktree }));
    let cli = procly.procly_in(&worktree, &["worktree", "release", "--json"]);
    assert_eq!(again, stdout(&cli));
}

#[test]
fn gc_lists_the_orphans_and_deletes_them_only_with_apply() {
    let procly = Procly::new();
    let (main, worktree) = running_worktree(&procly);
    assert_success(&procly.procly_in(&worktree, &["down", "web"]));
    git(&main, &["worktree", "remove", "--force", &worktree.to_string_lossy()]);
    let mut server = McpServer::start_in(&procly, &main);

    let listed = server.text("gc", json!({}));

    let cli = procly.procly_in(&main, &["gc", "--json"]);
    assert_success(&cli);
    assert_eq!(listed, stdout(&cli));
    assert!(listed.contains("\"applied\":false"), "{listed}");
    assert!(listed.contains("\"kind\":\"lease\""), "{listed}");
    let still_listed = server.text("gc", json!({ "apply": false }));
    assert_eq!(still_listed, listed);

    let applied = server.text("gc", json!({ "apply": true }));

    assert!(applied.contains("\"applied\":true"), "{applied}");
    let after = procly.procly_in(&main, &["gc", "--json"]);
    assert_success(&after);
    assert!(stdout(&after).contains("\"orphans\":[]"), "{}", stdout(&after));
}

#[test]
fn gc_apply_that_fails_to_delete_is_a_tool_error_with_exit_code_6() {
    let procly = Procly::new();
    let (main, worktree) = running_worktree(&procly);
    assert_success(&procly.procly_in(&worktree, &["down", "web"]));
    git(&main, &["worktree", "remove", "--force", &worktree.to_string_lossy()]);
    // The output directory cannot be taken out of a directory the Daemon may not write.
    let outputs = procly.state_dir().join("output");
    fs::set_permissions(&outputs, fs::Permissions::from_mode(0o500)).expect("make the output directory read-only");
    let mut server = McpServer::start_in(&procly, &main);

    let result = server.call("gc", json!({ "apply": true }));

    fs::set_permissions(&outputs, fs::Permissions::from_mode(0o700)).expect("make the output directory writable");
    assert_eq!(result["isError"], json!(true), "{result}");
    assert_eq!(result["structuredContent"]["exit_code"], json!(6), "{result}");
    assert!(
        result["structuredContent"]["stdout"]
            .as_str()
            .is_some_and(|document| document.contains("\"applied\":true")),
        "{result}"
    );
}

/// `procly` in `dir` with a home directory of its own and a fake `docker` first in the `PATH`, which has the Compose
/// plugin when `compose` is true. Nothing of the developer's machine takes part in what `doctor` checks.
fn doctor_command(procly: &Procly, dir: &Path, compose: bool) -> Command {
    let (home, bin) = (procly.scratch().join("home"), procly.scratch().join("bin"));
    fs::create_dir_all(&home).expect("create the home directory");
    fs::create_dir_all(&bin).expect("create the fake tools directory");
    let docker = bin.join("docker");
    let script = if compose {
        "#!/bin/sh\necho 'Docker Compose version v2.29.1'\n"
    } else {
        "#!/bin/sh\necho \"docker: 'compose' is not a docker command.\" >&2\nexit 1\n"
    };
    fs::write(&docker, script).expect("write the fake docker");
    fs::set_permissions(&docker, fs::Permissions::from_mode(0o755)).expect("make it executable");
    let mut command = procly.command();
    command
        .current_dir(dir)
        .env("HOME", &home)
        .env_remove("XDG_CONFIG_HOME")
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default()),
        );
    command
}

#[test]
fn doctor_that_finds_a_problem_is_a_tool_error_with_exit_code_7_and_the_document_of_the_cli() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let mut server = McpServer::start_command(doctor_command(&procly, &main, false));

    let result = server.call("doctor", json!({}));

    let mut cli = doctor_command(&procly, &main, false);
    let cli = cli.args(["doctor", "--json"]).output().expect("the procly binary runs");
    assert_eq!(cli.status.code(), Some(7), "{}", stderr(&cli));
    assert_eq!(result["isError"], json!(true), "{result}");
    assert_eq!(result["structuredContent"]["exit_code"], json!(7), "{result}");
    assert_eq!(result["structuredContent"]["stdout"], json!(stdout(&cli)), "{result}");
    assert!(stdout(&cli).contains("docker compose"), "{}", stdout(&cli));
}

#[test]
fn doctor_returns_the_json_of_the_cli_and_never_accepts_agent() {
    let procly = Procly::new();
    let main = git_repository(procly.scratch(), "app");
    let mut server = McpServer::start_command(doctor_command(&procly, &main, true));

    let result = server.call("doctor", json!({}));

    let mut cli = doctor_command(&procly, &main, true);
    let cli = cli.args(["doctor", "--json"]).output().expect("the procly binary runs");
    assert_eq!(document_of(&result), stdout(&cli));
    let given = server.call("doctor", json!({ "cwd": main }));
    assert_eq!(document_of(&given), stdout(&cli));

    let response = server.request(
        "tools/call",
        json!({ "name": "doctor", "arguments": { "agent": "claude" } }),
    );
    let refused = response.get("error").is_some() || response["result"]["isError"] == json!(true);
    assert!(refused, "doctor accepted --agent: {response}");
    let tools = server.request("tools/list", json!({}));
    let doctor = tools["result"]["tools"]
        .as_array()
        .expect("a list of tools")
        .iter()
        .find(|tool| tool["name"] == json!("doctor"))
        .expect("doctor is exposed")
        .clone();
    assert!(doctor["inputSchema"]["properties"].get("agent").is_none(), "{doctor}");
}

#[test]
fn the_server_ends_when_the_client_closes_its_input() {
    let procly = Procly::new();
    let mut server = McpServer::start(&procly);
    server.text("status", json!({}));

    server.stdin = None;

    let deadline = std::time::Instant::now() + DEADLINE;
    loop {
        if server.child.try_wait().expect("poll the server").is_some() {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "the server outlived its client");
        std::thread::sleep(Duration::from_millis(50));
    }
}
