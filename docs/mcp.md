# The MCP server

`procly mcp` is a stdio MCP server for agents that prefer MCP to a shell. It exposes the verbs of the Daemon as tools, and each tool returns exactly the document `--json` makes the verb print (ADR 0009). It holds no logic of its own, so behaviour cannot diverge between the CLI and MCP.

Procly never writes into the configuration of an agent. You register the server yourself, with the command your agent gives you:

```console
$ claude mcp add procly -- procly mcp
$ codex mcp add procly -- procly mcp
```

Any other MCP client takes the same command and arguments in its server list:

```json
{ "mcpServers": { "procly": { "command": "procly", "args": ["mcp"] } } }
```

A tool runs its command with the environment of the `procly mcp` process, so the Jobs and Services it starts get that environment and not the one of the shell of your agent. Set what they need in the `env` block of the server entry of your agent.

The server starts the Daemon on demand, on the first tool call, like any other command. It ends when the client closes its input. It gives a client no ownership of the Services it starts, see ADR 0010.

## Tools

Each tool takes the arguments of its CLI verb, named like them, plus an optional `cwd`, the directory to run in. Without `cwd` a tool runs in the directory `procly mcp` was started from, so one server can serve several Worktrees.

| Tool | CLI verb | Arguments |
| --- | --- | --- |
| `run` | `procly run` | `command`, `timeout`, `cwd` |
| `wait` | `procly wait` | `job`, `cwd` |
| `logs` | `procly logs` | `job_or_service`, `cwd` |
| `up` | `procly up` | `service`, `gitignore`, `cwd` |
| `down` | `procly down` | `service`, `cwd` |
| `env` | `procly env` | `service`, `cwd` |
| `status` | `procly status` | `all`, `cwd` |
| `worktree_init` | `procly worktree init` | `path`, `cwd` |

`init`, `unit`, `guard`, `agent`, `off`, `on`, `eject` and `worktree hooks` have no tool. They write into the Repository or into the session configuration of the person, and an MCP client must not. A call to one of them is refused.

`wait` and `logs` return in one response, when the Job has ended for `wait`. A client with a request timeout shorter than the Job should call `wait` on Jobs it expects to be short, or poll `logs`.

## What a tool returns

When the verb exits with 0, the tool returns one block of text, the document of `procly <verb> --json`, byte for byte. See [JSON output](json.md).

```console
$ procly status --json
{"schema_version":1, …}
```

When the verb exits with another code, the tool returns an MCP tool error, so that the agent can act on a Port mismatch or a timeout as it would from a shell, see [exit codes](exit-codes.md). The first block of text names the verb, the exit code and the stderr of the verb. The result also carries `structuredContent` with the same facts apart:

```json
{ "exit_code": 124, "stderr": "procly: job 3f9a1c2e timed out after 1s\n", "stdout": "{\"schema_version\":1, …}\n" }
```

`stdout` holds the document the verb printed before it failed, when it printed one. A Job that exits with 3 makes `wait` a tool error with `exit_code` 3 and the document of the Job in `stdout`, the same as the CLI exits with 3 and prints that document.
