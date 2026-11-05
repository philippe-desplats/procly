# Restart replays the environment of the `up` that started the Service, held in memory only

A Service runs with the environment of the `up` that started it, the caller's `PATH`, nvm, mise or direnv included, with the variables of its Leases on top. The desktop app has no shell of its own. Opened from the Dock, it inherits the environment of launchd, whose `PATH` is `/usr/bin:/bin:/usr/sbin:/sbin`, so an `up` sent with it would start a Service that finds neither its runtime nor `docker`. Capturing the login shell of the developer from the app was rejected: it runs the developer's rc files from a GUI process, is slow, can block on a prompt, and puts logic in a client that holds none (ADR 0009).

The Daemon therefore keeps, in memory, the environment of the last `up` of each Service, and a new request, `Restart`, starts the Service again with it. A Restart is the one way the app starts a Service, and it replays exactly what the agent or the developer gave the first time.

- The held environment is replaced by every `up` of the Service, kept after the Service ends, and dropped when the Daemon stops and on the Worktree release of its Worktree. It is never written to disk and never returned by `Status`, since it may carry secrets. A re-attached Service has none, as the environment of the previous Daemon is gone.
- `ServiceStatus` carries a `restartable` boolean, true when the Daemon holds an environment for the Service. Without one, the app offers the command to copy, `(cd <path> && procly restart <service>)`, which carries the shell of the developer.
- A Restart stops the Service if it runs, then starts it with the held environment and the same Lease, and answers once it is healthy, as `Up` does. It starts the needs that do not run, as `Up` does, and never restarts the ones that do. A Service a Group covers is restarted through its Group. An Isolated Compose service follows the path of `down` then `up`.
- The CLI gains `procly restart <service>`, the name of the Service being required. Its text, exit codes and `--json` document are those of an `up` of one Service. `procly mcp` does not expose it.
- `Logs` of a Compose service runs `docker compose` with the held environment of the Service, and with the environment of the request when the Daemon holds none.

The app does not start a Service nothing started (no `up` of its own, no "start all"), since it has no environment to give it.

## Consequences

- The automatic restart of a Service that crashed (`restart_attempts`) and a Restart are distinct. The first never starts the needs again, the second starts those that do not run.
- A Restart after the Daemon itself restarted is not available until the next `up`, from the app or from a shell.
- The Daemon holds one environment per Service in memory for as long as it lives, which it already does for a running Service, and now does for an ended one too.
- The new request and the `restartable` field are part of protocol 22.
