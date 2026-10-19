# JSON output

Every command that prints something accepts `--json` and then prints exactly one JSON document on stdout, so that an agent parses it instead of scraping text (ADR 0009). The only commands without it are `guard`, which answers a Claude Code hook in its own JSON, and `daemon`, which prints nothing.

**The schema version is 1.** It is the `schema_version` of every document, the one `procly status --json` prints, and this is the only place that lists it. It is bumped on any incompatible change to any document, independently of the socket protocol between the CLI and the Daemon. Adding a key or a document is not incompatible, so a reader ignores the keys it does not know. Removing a key, renaming one or changing what it means bumps the version.

## What `--json` changes

- Stdout holds one line of JSON, which starts with `schema_version` and ends with a newline. Without `--json` the text output is what it always was.
- The exit code is the same with and without `--json`, see [exit codes](exit-codes.md). A conflict, a Port mismatch or a Service that did not become healthy still exits with its own code, and its document says why.
- A command that fails still reports on stderr, as without `--json`: the Daemon that cannot be reached, a Service that is not declared, the diagnostic of an `up` that ends unhealthy or in conflict. A failure of Procly itself prints no document at all.
- The lines of stderr that only tell what the document carries are not printed, such as `started` and `reused` after `run`, the `(needed by …)` lines of `up`, `wrote …` after `init`, and the notes about a Managed block left as it is.
- The document carries what the text says and no more. Paths are absolute, durations are in milliseconds, and output is the text the process printed.

## Commands

| Command | Document | Documented in |
| --- | --- | --- |
| `run` | the Job and the outcome | below |
| `wait` | how the Job ended, its exit code and its output | below |
| `logs` | the recent output | below |
| `status` | the Repositories, Worktrees, Services and Jobs | below |
| `up` | the Services reached, or why one was not | below, and [Configuration](configuration.md#procly-up-without-arguments) |
| `down` | the Services stopped and the Compose services let go of | below |
| `env` | the variables of a Service | [`procly env`](configuration.md#procly-env) |
| `init` | the proposal and whether it was written | [Proposing a configuration](init.md) |
| `worktree init` | what was copied and what ran | [Preparing a Worktree](worktree-init.md) |
| `agent install`, `off`, `on` | what became of the Rules block and the Guard | [Steering agents to Procly](agents.md) |
| `eject` | what was stopped, removed and left, and the prompt | [Removing Procly from a Repository](eject.md) |
| `unit install`, `unit inspect`, `unit remove` | the Session unit | [The Daemon](daemon.md) |

## `run`

```console
$ procly run --json "bun test"
{"schema_version":1,"job":"3f9a1c2e","outcome":"started"}
```

`outcome` is `started`, or `reused` when the same command already runs in the same Worktree and directory.

## `wait`

```console
$ procly wait --json 3f9a1c2e
{"schema_version":1,"job":"3f9a1c2e","exit_code":3,"end":{"kind":"exited","code":3},"output":"hi\n"}
```

`exit_code` is the code `wait` exits with: the Job's own, 128 + N for a Job killed by signal N, and 124 for a Job killed on its timeout. `end` tells how the Job ended, with a `kind` of `exited` (`code`), `signaled` (`signal`) or `timed_out` (`timeout_ms`). `output` is everything the Job printed.

## `logs`

```console
$ procly logs --json 3f9a1c2e
{"schema_version":1,"name":"3f9a1c2e","output":"hi\n"}
```

`name` is the Job or the Service asked for.

## `up`

A healthy `up` lists the Services it reached in the order they came up, the Services the one you asked for needs first. `needed_by` names the Service that needs it, and is `null` for a Service you asked for. `port` is the Lease the Service listens on and `url` where to reach it, both `null` for a Service without a port. `via` is the Group that runs a covered Service, or `null`.

```console
$ procly up --json web
{"schema_version":1,"result":"healthy","services":[{"service":"api","outcome":"started","via":null,"port":4100,"url":"http://localhost:4100","needed_by":"web"},{"service":"web","outcome":"started","via":null,"port":4101,"url":"http://localhost:4101","needed_by":null}]}
```

`procly up --json` without a Service lists every Service it brought up, all with a `needed_by` of `null`.

A Service that is not healthy has a `result` of `unhealthy`. The exit code says which kind it is (3, 4 or 124), and `end` tells why, with a `kind` of `exited` (`code`), `signaled` (`signal`), `health_timeout` (`timeout_ms`), `stopped`, `idle` (`timeout_ms`), `failed` (`restarts`), `port_mismatch` (`leased`, `actual`), `lost`, `orphaned` (`pgid`, `port`) or `not_listening` (`port`). `output` is the end of its output, the last 20 lines the text prints on stderr.

```console
$ procly up --json web
{"schema_version":1,"result":"unhealthy","service":"web","needed_by":null,"end":{"kind":"exited","code":7},"output":"cannot bind\n"}
$ echo $?
3
```

A Service whose Leases or container name are held outside Procly has a `result` of `conflict` and exits with 5. Each conflict says what is held (`held`, with a `kind` of `port`, `container_name` or `service`), what holds it (`occupant`, with a `kind` of `container`, `process` or `unknown`) and the command that frees it, which Procly never runs (`stop`, or `null` when the occupant cannot be named).

```json
{"schema_version":1,"result":"conflict","service":"db","needed_by":null,"conflicts":[{"held":{"kind":"port","port":5432},"occupant":{"kind":"container","name":"old-db","project":null,"service":null},"stop":"docker stop old-db"}]}
```

## `down`

```console
$ procly down --json
{"schema_version":1,"service":null,"stopped":["api","web"],"released":[]}
```

`service` is the Service asked for, or `null` for all of them. `released` lists the Compose services the Worktree let go of, each with `service`, `others` (how many other Worktrees still need the container) and `grace_ms` (how long it keeps running once none does).

## `status`

```console
$ procly status --json
{"schema_version":1,"repositories":[{"git_common_dir":"/home/me/app/.git","worktrees":[{"path":"/home/me/app","slug":"app","main":true,"services":[{"service":"web","port":4101,"reattached":false,"state":"healthy"}],"jobs":[{"id":"3f9a1c2e","command":"bun test","cwd":"/home/me/app","state":"ended","end":{"kind":"exited","code":3}}]}]}],"jobs_outside_repositories":[]}
```

A Service has a `state` of `starting`, `healthy`, `restarting` or `ended`, and an ended one carries an `end` like the one of `up`. A Worktree lists its Foreign processes under `foreign` when it has some, see [Foreign processes](daemon.md#foreign-processes).
