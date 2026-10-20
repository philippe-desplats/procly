# The CLI is the contract, MCP and the desktop app are logic-free clients

Every coding agent can run a shell command, but not every agent handles MCP the same way. The CLI is therefore the reference interface. `procly mcp` and the later Tauri app are thin clients of the Daemon socket that expose the same verbs and hold no logic of their own, so behaviour can never diverge between surfaces.

## Exceptions

- `procly unit` runs in the client without asking the Daemon, since the Session unit is what starts the Daemon. It reads and writes the unit file itself, and its `--json` document is built by the client like the others.
- `procly init` also runs in the client and never reaches the Daemon, see ADR 0013.
- `--json` may carry more than the text. The text stays what it always was, while the document adds what a script needs to parse, such as the `ports` object of `up` and the `installed`, `deleted` and `needed_by` fields. Adding a key is not an incompatible change, see `docs/json.md`.
