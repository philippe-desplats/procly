# init runs in the CLI and never reaches the Daemon, and the up line names every Lease

`procly init` reads the Worktree and proposes a `procly.toml`, and nothing it reads or writes belongs to the Daemon: no Lease, no Service, no state directory. It therefore runs in the client, like `procly unit` (ADR 0009), so it never starts the Daemon. This is the second exception to the rule that the CLI is a thin client of the socket, and it has a consequence for the other surfaces: `procly mcp` and the desktop app get no `init` verb on the socket, since there is no request for them to forward.

The line `procly up` prints names every Lease of the Service, no longer the port of the health check alone. The Lease the health check probes comes first, then the others by Port name. A Lease prints as `<url> port <n>` when the scheme of its port is known, and as `<port name> port <n>` otherwise, never as an `http` URL for a port that may not speak HTTP. The scheme of a Lease is the first that applies: the one of the URL of the Driver of the Service, then the Port name itself when it is `amqp`, `http`, `https`, `redis` or `smtp`, then `http` for the Lease the health check of a Process service probes or the Lease of an HTTP health check. A Port name that is a scheme therefore wins over the `http` of the health port of a Process service: a Process service whose only Port name is `https` prints an `https` URL.

## Consequences

- Among the verbs an agent or a script types, `init` and `unit` are the only ones the client answers alone. A new verb that needs no Daemon is a third exception to record, any other goes through the socket.
- The text of the `up` line changed, and the CLI is the contract (ADR 0009), so `docs/configuration.md` records the change. A script that reads the port of the health check reads the number before the first comma.
- `--json` carries the same URLs in `url` and `urls`, see `docs/json.md`.
