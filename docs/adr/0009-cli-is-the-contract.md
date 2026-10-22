# The CLI is the contract, MCP and the desktop app are logic-free clients

Every coding agent can run a shell command, but not every agent handles MCP the same way. The CLI is therefore the reference interface. `procly mcp` and the later Tauri app are thin clients that expose the same verbs and hold no logic of their own, so behaviour can never diverge between surfaces. `procly mcp` is a thin client of the CLI itself: each tool runs the same `procly` executable with `<verb> --json` and returns its output unchanged, and the Tauri app is planned as a thin client of the Daemon socket.

## Exceptions

- `procly unit` runs in the client without asking the Daemon, since the Session unit is what starts the Daemon. It reads and writes the unit file itself, and its `--json` document is built by the client like the others.
- `procly init` also runs in the client and never reaches the Daemon, see ADR 0013.
- `procly doctor` runs its checks of the installation in the client and asks the Daemon only for the checks only the Daemon can answer, so that it still reports a Daemon that does not run. It never starts the Daemon.
- `--json` may carry more than the text. The text stays what it always was, while the document adds what a script needs to parse, such as the `ports` object of `up` and the `installed`, `deleted` and `needed_by` fields. Adding a key is not an incompatible change, see `docs/json.md`.
- `procly mcp` runs one CLI process per tool call instead of sending Daemon socket requests in process, because every verb prints its own output in the CLI and reusing it in process would mean rewriting each verb into a print-free function. The `--json` document, the exit code and the on-demand start of the Daemon are therefore those of the CLI by construction. The costs are 1 process per call, a Job that gets the environment of the `procly mcp` process rather than the agent's shell, and an executable path fixed when the server starts, so the server is restarted after an upgrade.
