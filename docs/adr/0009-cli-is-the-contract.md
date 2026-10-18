# The CLI is the contract, MCP and the desktop app are logic-free clients

Every coding agent can run a shell command, but not every agent handles MCP the same way. The CLI is therefore the reference interface. `procly mcp` and the later Tauri app are thin clients of the Daemon socket that expose the same verbs and hold no logic of their own, so behaviour can never diverge between surfaces.
