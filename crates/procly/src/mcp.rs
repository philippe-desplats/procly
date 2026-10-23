//! `procly mcp`: a stdio MCP server whose tools are the verbs of the CLI (ADR 0009).
//!
//! The server holds no logic of its own. Each tool runs the `procly` executable it is part of with the arguments of
//! the CLI verb and `--json`, in the working directory the caller names, and hands back the document the verb
//! printed. Behaviour cannot diverge between the 2 surfaces because there is one implementation, and the Daemon
//! starts on demand on the first tool call like for any other command.

use std::os::unix::process::ExitStatusExt as _;
use std::path::PathBuf;
use std::process::{ExitStatus, Stdio};

use anyhow::Context as _;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::schemars::{self, JsonSchema};
use rmcp::{ErrorData, ServerHandler, ServiceExt as _, tool, tool_handler, tool_router};
use serde::Deserialize;
use serde_json::json;

/// What `initialize` tells the client about using the tools.
const INSTRUCTIONS: &str = "Procly supervises the dev processes and containers of a Worktree, so that parallel \
                            agents never duplicate a process, a container or a port. Each tool takes the arguments of \
                            its `procly` command and returns the document of `procly <command> --json`. A command \
                            that fails returns a tool error carrying its exit code and its stderr.";

/// The exit code of a command that failed in Procly itself, reported when the command left no code of its own.
const EXIT_PROCLY_FAILURE: i32 = crate::EXIT_PROCLY_FAILURE as i32;

/// Starts the server on the standard input and output, and returns when the client closes its input.
pub(crate) fn serve() -> anyhow::Result<()> {
    let exe = std::env::current_exe().context("cannot locate the procly executable")?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("cannot start the async runtime")?;
    let result = runtime.block_on(async {
        let running = McpServer::new(exe)
            .serve(rmcp::transport::stdio())
            .await
            .context("cannot start the MCP server")?;
        running.waiting().await.context("the MCP server stopped")?;
        Ok(())
    });
    // A command still running for a request nobody awaits any more is not worth waiting for.
    runtime.shutdown_background();
    result
}

/// The arguments of `procly run`.
#[derive(Debug, Deserialize, JsonSchema)]
struct RunArguments {
    /// The command, run by `/bin/sh -c`.
    command: String,
    /// Kill the Job's whole process tree once it has run this long, as a number followed by `ms`, `s`, `m`, `h` or
    /// `d` (`500ms`, `90s`, `5m`, `1h`), a bare number meaning seconds. Defaults to `30m`.
    timeout: Option<String>,
    /// The directory to run in, inside the Worktree. Defaults to the directory `procly mcp` was started from.
    cwd: Option<PathBuf>,
}

/// The arguments of `procly wait`.
#[derive(Debug, Deserialize, JsonSchema)]
struct WaitArguments {
    /// The identifier of the Job, as `run` returned it.
    job: String,
    /// The directory to run in. Defaults to the directory `procly mcp` was started from.
    cwd: Option<PathBuf>,
}

/// The arguments of `procly logs`.
#[derive(Debug, Deserialize, JsonSchema)]
struct LogsArguments {
    /// A Job identifier printed by `run`, or the name of a Service of the Worktree.
    job_or_service: String,
    /// The directory to run in, inside the Worktree. Defaults to the directory `procly mcp` was started from.
    cwd: Option<PathBuf>,
}

/// The arguments of `procly up`.
#[derive(Debug, Deserialize, JsonSchema)]
struct UpArguments {
    /// The Service to start. Without one, every declared Service no Group covers is started.
    service: Option<String>,
    /// Exclude the files Procly generates through the `.gitignore` of the Worktree, as a Managed block, instead of
    /// the git exclude file shared by the Repository.
    gitignore: Option<bool>,
    /// The directory to run in, inside the Worktree. Defaults to the directory `procly mcp` was started from.
    cwd: Option<PathBuf>,
}

/// The arguments of `procly down`.
#[derive(Debug, Deserialize, JsonSchema)]
struct DownArguments {
    /// The Service to stop. Without one, every Service of the Worktree is stopped.
    service: Option<String>,
    /// The directory to run in, inside the Worktree. Defaults to the directory `procly mcp` was started from.
    cwd: Option<PathBuf>,
}

/// The arguments of `procly env`.
#[derive(Debug, Deserialize, JsonSchema)]
struct EnvArguments {
    /// The Service whose variables are returned.
    service: String,
    /// The directory to run in, inside the Worktree. Defaults to the directory `procly mcp` was started from.
    cwd: Option<PathBuf>,
}

/// The arguments of `procly status`.
#[derive(Debug, Deserialize, JsonSchema)]
struct StatusArguments {
    /// Report every Worktree of every Repository the Daemon knows, not only the current one.
    all: Option<bool>,
    /// The directory to run in, inside the Worktree. Defaults to the directory `procly mcp` was started from.
    cwd: Option<PathBuf>,
}

/// The arguments of `procly worktree init`.
#[derive(Debug, Deserialize, JsonSchema)]
struct WorktreeInitArguments {
    /// The Worktree to prepare, or any directory in it. Defaults to `cwd`.
    path: Option<PathBuf>,
    /// The directory to run in. Defaults to the directory `procly mcp` was started from.
    cwd: Option<PathBuf>,
}

/// The arguments of `procly worktree release`.
#[derive(Debug, Deserialize, JsonSchema)]
struct WorktreeReleaseArguments {
    /// The Worktree to release, or any directory in it. Defaults to `cwd`.
    path: Option<PathBuf>,
    /// The directory to run in. Defaults to the directory `procly mcp` was started from.
    cwd: Option<PathBuf>,
}

/// The arguments of `procly gc`.
#[derive(Debug, Deserialize, JsonSchema)]
struct GcArguments {
    /// Delete the Orphans instead of listing them. Without it nothing is deleted.
    apply: Option<bool>,
    /// The directory to run in. Defaults to the directory `procly mcp` was started from.
    cwd: Option<PathBuf>,
}

/// The arguments of `procly doctor`. It has no `agent`: the hand-off to a coding agent is the person's to ask for, and
/// a call carrying it is refused.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DoctorArguments {
    /// The directory to run in, whose Worktree is checked. Defaults to the directory `procly mcp` was started from.
    cwd: Option<PathBuf>,
}

/// The server: one tool per verb the CLI exposes, and the executable they run.
///
/// `init`, `unit`, `guard`, `agent`, `off`, `on`, `eject` and `worktree hooks` have no tool: they write into the Repository or the
/// session configuration of the person, which an MCP client must not do.
#[derive(Debug, Clone)]
struct McpServer {
    exe: PathBuf,
    tool_router: ToolRouter<Self>,
}

impl McpServer {
    fn new(exe: PathBuf) -> Self {
        Self {
            exe,
            tool_router: Self::tool_router(),
        }
    }

    /// Runs `procly <verb…> --json <flags…> -- <values…>` in `cwd` and turns what it printed into a tool result. The
    /// values follow `--` so that one starting with a dash, such as a shell command, is never read as a flag.
    async fn invoke(
        &self,
        verb: &[&str],
        flags: Vec<String>,
        values: Vec<String>,
        cwd: Option<PathBuf>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut command = tokio::process::Command::new(&self.exe);
        command
            .args(verb)
            .arg("--json")
            .args(flags)
            // The standard input is the channel of the client, a command must never read from it.
            .stdin(Stdio::null())
            .kill_on_drop(true);
        if !values.is_empty() {
            command.arg("--").args(values);
        }
        if let Some(cwd) = &cwd {
            command.current_dir(cwd);
        }
        let name = verb.join(" ");
        let output = match command.output().await {
            Ok(output) => output,
            Err(source) => {
                let place = cwd.map_or_else(|| "the current directory".to_owned(), |cwd| cwd.display().to_string());
                return Ok(failure(
                    &name,
                    EXIT_PROCLY_FAILURE,
                    &format!("cannot run procly in {place}: {source}"),
                    "",
                ));
            }
        };
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        if output.status.success() {
            return Ok(CallToolResult::success(vec![ContentBlock::text(stdout)]));
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        Ok(failure(&name, exit_code(output.status), &stderr, &stdout))
    }
}

/// The exit code of a command that ended with `status`, as a shell reports it: its own, or 128 plus the signal that
/// killed it.
fn exit_code(status: ExitStatus) -> i32 {
    status
        .code()
        .or_else(|| status.signal().map(|signal| 128 + signal))
        .unwrap_or(EXIT_PROCLY_FAILURE)
}

/// The tool error of the verb `name` that exited with `code`, `stderr` being what it told and `stdout` the document
/// it may have printed before failing, as `wait` does for a Job that exited with a code.
fn failure(name: &str, code: i32, stderr: &str, stdout: &str) -> CallToolResult {
    let told = stderr.trim_end();
    let message = if told.is_empty() {
        format!("procly {name} exited with code {code}")
    } else {
        format!("procly {name} exited with code {code}:\n{told}")
    };
    let mut content = vec![ContentBlock::text(message)];
    if !stdout.is_empty() {
        content.push(ContentBlock::text(stdout));
    }
    let mut result = CallToolResult::error(content);
    result.structured_content = Some(json!({ "exit_code": code, "stderr": stderr, "stdout": stdout }));
    result
}

#[tool_router]
impl McpServer {
    #[tool(
        description = "Start a Job running a shell command in the Worktree and return its identifier and `started`, \
                       or `reused` when the same command already runs in the same Worktree and directory. Same as \
                       `procly run --json`."
    )]
    async fn run(&self, Parameters(arguments): Parameters<RunArguments>) -> Result<CallToolResult, ErrorData> {
        let mut flags = Vec::new();
        if let Some(timeout) = arguments.timeout {
            flags.extend(["--timeout".to_owned(), timeout]);
        }
        self.invoke(&["run"], flags, vec![arguments.command], arguments.cwd)
            .await
    }

    #[tool(
        description = "Wait for a Job to end and return how it ended, its exit code and its output in one response. \
                       A Job that exits with a code other than 0 or times out is a tool error carrying that code. \
                       Same as `procly wait --json`."
    )]
    async fn wait(&self, Parameters(arguments): Parameters<WaitArguments>) -> Result<CallToolResult, ErrorData> {
        self.invoke(&["wait"], Vec::new(), vec![arguments.job], arguments.cwd)
            .await
    }

    #[tool(
        description = "Return the recent output of a running or ended Job, or of a Service of the Worktree, in one \
                       response. Same as `procly logs --json`."
    )]
    async fn logs(&self, Parameters(arguments): Parameters<LogsArguments>) -> Result<CallToolResult, ErrorData> {
        self.invoke(&["logs"], Vec::new(), vec![arguments.job_or_service], arguments.cwd)
            .await
    }

    #[tool(
        description = "Start a declared Service of the Worktree, or find it running, wait until it is healthy and \
                       return where to reach it. Without a Service, start every declared Service no Group covers. \
                       Same as `procly up --json`."
    )]
    async fn up(&self, Parameters(arguments): Parameters<UpArguments>) -> Result<CallToolResult, ErrorData> {
        let flags = if arguments.gitignore == Some(true) {
            vec!["--gitignore".to_owned()]
        } else {
            Vec::new()
        };
        self.invoke(&["up"], flags, arguments.service.into_iter().collect(), arguments.cwd)
            .await
    }

    #[tool(
        description = "Stop a Service of the Worktree, or all of its Services without an argument, with their whole \
                       process trees. Same as `procly down --json`."
    )]
    async fn down(&self, Parameters(arguments): Parameters<DownArguments>) -> Result<CallToolResult, ErrorData> {
        self.invoke(
            &["down"],
            Vec::new(),
            arguments.service.into_iter().collect(),
            arguments.cwd,
        )
        .await
    }

    #[tool(
        description = "Return the variables Procly injects into a Service of the Worktree: its Leases, its Derived \
                       variables, `PROCLY=1` and `PROCLY_SERVICE`, without starting it. Same as `procly env --json`."
    )]
    async fn env(&self, Parameters(arguments): Parameters<EnvArguments>) -> Result<CallToolResult, ErrorData> {
        self.invoke(&["env"], Vec::new(), vec![arguments.service], arguments.cwd)
            .await
    }

    #[tool(
        description = "Return the Repository, the current Worktree, its Services and its Jobs. Same as `procly \
                       status --json`."
    )]
    async fn status(&self, Parameters(arguments): Parameters<StatusArguments>) -> Result<CallToolResult, ErrorData> {
        let flags = if arguments.all == Some(true) {
            vec!["--all".to_owned()]
        } else {
            Vec::new()
        };
        self.invoke(&["status"], flags, Vec::new(), arguments.cwd).await
    }

    #[tool(
        description = "Make a fresh Worktree ready: copy the untracked files listed in `[worktree] copy` from the \
                       Main worktree when the Worktree lacks them, then run the `[worktree] setup` commands that \
                       have not succeeded yet. Same as `procly worktree init --json`."
    )]
    async fn worktree_init(
        &self,
        Parameters(arguments): Parameters<WorktreeInitArguments>,
    ) -> Result<CallToolResult, ErrorData> {
        let values = arguments
            .path
            .map(|path| path.to_string_lossy().into_owned())
            .into_iter()
            .collect();
        self.invoke(&["worktree", "init"], Vec::new(), values, arguments.cwd)
            .await
    }

    #[tool(
        description = "Release a Worktree before it is removed: stop its Services and Jobs with their whole process \
                       trees, let go of its Compose services as `down` does and release its Leases, except those of \
                       a Pinned branch. Nothing is deleted, the Logical resources are kept. Releasing again returns \
                       `nothing to release`. Same as `procly worktree release --json`."
    )]
    async fn worktree_release(
        &self,
        Parameters(arguments): Parameters<WorktreeReleaseArguments>,
    ) -> Result<CallToolResult, ErrorData> {
        let values = arguments
            .path
            .map(|path| path.to_string_lossy().into_owned())
            .into_iter()
            .collect();
        self.invoke(&["worktree", "release"], Vec::new(), values, arguments.cwd)
            .await
    }

    #[tool(
        description = "List the Orphans, what removed Worktrees left behind: Leases, Logical resources, Isolated \
                       containers and volumes, and output and setup directories. With `apply`, delete them all, \
                       going on after a failure, which is a tool error with exit code 6 carrying the report. Without \
                       it nothing is deleted. Same as `procly gc --json`."
    )]
    async fn gc(&self, Parameters(arguments): Parameters<GcArguments>) -> Result<CallToolResult, ErrorData> {
        let flags = if arguments.apply == Some(true) {
            vec!["--apply".to_owned()]
        } else {
            Vec::new()
        };
        self.invoke(&["gc"], flags, Vec::new(), arguments.cwd).await
    }

    #[tool(
        description = "Check that Procly can work here and return every check with its result and the command that \
                       fixes a problem. Nothing is changed or run. A check that found a problem is a tool error with \
                       exit code 7 carrying the document. Same as `procly doctor --json`."
    )]
    async fn doctor(&self, Parameters(arguments): Parameters<DoctorArguments>) -> Result<CallToolResult, ErrorData> {
        self.invoke(&["doctor"], Vec::new(), Vec::new(), arguments.cwd).await
    }
}

#[expect(
    clippy::unused_async_trait_impl,
    reason = "the macro writes the async handler methods, the tools themselves await the command"
)]
#[tool_handler(router = self.tool_router)]
impl ServerHandler for McpServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("procly", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }
}
