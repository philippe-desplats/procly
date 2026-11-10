# JSON output

Every command that prints something accepts `--json` and then prints exactly 1 JSON document on stdout, so that an agent parses it instead of scraping text (ADR 0009). The only commands without it are `guard`, which answers a Claude Code hook in its own JSON, `mcp`, which speaks JSON-RPC and returns these documents as tool results, and `daemon` without a subcommand, which runs the Daemon in the foreground and prints nothing.

The one exception to 1 document is `logs --follow`, which streams and prints 1 JSON document per line, see [`logs --follow`](#logs---follow).

**The schema version is 1.** It is the `schema_version` of every document, the one `procly status --json` prints. This page is where it is defined, the examples in the other docs repeat the value. It is bumped on any incompatible change to any document, independently of the socket protocol between the CLI and the Daemon. Adding a key or a document is not incompatible, so a reader ignores the keys it does not know. Removing a key, renaming one or changing what it means bumps the version.

## What `--json` changes

- Stdout holds 1 line of JSON, which starts with `schema_version` and ends with a newline. Without `--json` the text output is what it always was.
- The exit code is the same with and without `--json`, see [exit codes](exit-codes.md). A conflict, a Port mismatch or a Service that did not become healthy still exits with its own code, and its document says why.
- A command that fails still reports on stderr, as without `--json`: the Daemon that cannot be reached, a Service that is not declared, the diagnostic of an `up` that ends unhealthy or in conflict. A failure of Procly itself prints no document at all. The progress of a long setup command of `worktree init` is written to stderr too, and stdout keeps its single document.
- The lines of stderr that only tell what the document carries are not printed, such as `started` and `reused` after `run`, the `(needed by …)` lines of `up`, `wrote …` after `init`, and the notes about a Managed block left as it is.
- The document carries what the text says, and `up` adds every Lease of a Service under `ports`, which the text leaves to `procly env`. Paths are absolute, durations are in milliseconds, and output is the text the process printed.

## Commands

| Command | Document | Documented in |
| --- | --- | --- |
| `run` | the Job and the outcome | below |
| `wait` | how the Job ended, its exit code and its output | below |
| `logs` | the recent output | below |
| `logs --follow` | 1 document per line: the backlog, the output as it arrives, what was dropped, the end | below |
| `status` | the Repositories, Worktrees, Services and Jobs | below |
| `up` | the Services reached, or why one was not | below, and [Configuration](configuration.md#procly-up-without-arguments) |
| `down` | the Services stopped and the Compose services let go of | below |
| `restart` | the Services reached, or why one was not, as for `up` | [below](#restart) |
| `env` | the variables of a Service | [`procly env`](configuration.md#procly-env) |
| `init` | the proposal and whether it was written | [Proposing a configuration](init.md) |
| `worktree init` | what was copied and what ran | [Preparing a Worktree](worktree-init.md) |
| `worktree release` | what the Worktree release stopped, released and kept, or would, with `--dry-run` | [Releasing a Worktree](worktree-release.md#json) |
| `worktree hooks` | what became of the Worktrunk hooks block | [below](#worktree-hooks), and [Preparing a Worktree](worktree-init.md#worktrunk) |
| `agent install`, `off`, `on` | what became of the Rules block and the Guard | [Steering agents to Procly](agents.md) |
| `eject` | what was stopped, removed, left and spared, and the prompt | [Removing Procly from a Repository](eject.md) |
| `gc` | the Orphans, and what became of each with `--apply` | below, and [Collecting Orphans](gc.md) |
| `doctor` | every check with its status, message and fix | below, and [Checking the installation](doctor.md) |
| `unit install`, `unit inspect`, `unit remove` | the Session unit | [The Daemon](daemon.md) |
| `daemon stop` | whether a Daemon was stopped, its pid and how long it took | [below](#daemon-stop), and [The Daemon](daemon.md#stopping-the-daemon) |
| `mcp` | none, it serves the documents above as MCP tools | [The MCP server](mcp.md) |

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

`exit_code` is the code `wait` exits with: the Job's own, 128 + N for a Job killed by signal N, and 124 for a Job killed on its timeout. `end` tells how the Job ended, with a `kind` of `exited` (`code`), `signaled` (`signal`) or `timed_out` (`timeout_ms`). `output` is everything the Job printed. A Job that ended long ago may be unknown by now, see [ended Jobs](daemon.md#ended-jobs): `wait` then fails with exit code 125 and `no job 3f9a1c2e` on stderr, as for an identifier that never existed.

## `logs`

```console
$ procly logs --json 3f9a1c2e
{"schema_version":1,"name":"3f9a1c2e","output":"hi\n"}
```

`name` is the Job or the Service asked for. The `output` of a Compose service is the last 1000 lines of its container, as `docker compose logs` prints them. The `output` of an ended Job is no longer there once the Daemon has evicted the Job, see [ended Jobs](daemon.md#ended-jobs).

## `logs --follow`

With `--follow`, `logs` prints NDJSON: 1 JSON document per line, the one exception to 1 document per command. Every line is self-contained, it carries `schema_version`, `name` and `event`, and a reader parses each line on its own as it arrives.

```console
$ procly logs --follow --json api
{"schema_version":1,"name":"api","event":"backlog","output":"listening on 4100\n"}
{"schema_version":1,"name":"api","event":"output","output":"GET /health 200\n"}
{"schema_version":1,"name":"api","event":"dropped","bytes":4096}
{"schema_version":1,"name":"api","event":"ended"}
```

- `backlog` comes first, once, with the recent output as `logs --json` gives it.
- `output` carries what the Job or the Service printed since the line before. A Service that crashes goes on in the same stream through its automatic restart, the note of the restart included.
- `dropped` tells how many bytes of output this reader missed because it read too slowly. The Daemon never waits for a reader, it drops what does not fit in the queue of the connection instead.
- `ended` comes last, once the Job or the Service has ended and all of its output was printed, and `logs` then exits with 0.

Ctrl-C also exits with 0 but prints no `ended` line, so a reader tells a real end from an interruption. A Compose service cannot be followed: `logs --follow` prints no document, exits with 125 and names the `docker compose logs -f` command to run on stderr.

## `up`

A healthy `up` lists the Services it reached in the order they came up, the Services the one you asked for needs first. `needed_by` names the Service that needs it, and is `null` for a Service you asked for. `port` is the Lease the Service listens on and `url` the URL of that Lease, both `null` for a Service without a port. `url` is also `null` when Procly does not know the scheme of that port, see [What `procly up` prints](configuration.md#what-procly-up-prints). `ports` holds every Lease of the Service by Port name, such as `{"admin":4102,"http":4101}` for a Service declaring `http` and `admin`, and is empty for a Service without a port. `port` is always one of them, the one health check probes. `urls` holds the URL of each Lease whose scheme Procly knows, by Port name, such as `{"http":"http://localhost:8025","smtp":"smtp://localhost:1025"}` for a Mailpit, and leaves the others out. `via` is the Group that runs a covered Service, or `null`. `warnings` lists what went wrong for that Service without failing `up`, or what Procly cannot tell, and is empty when nothing did. It holds the [Portless alias](configuration.md#portless-aliases) that could not be made, and the doubt about a Service a previous Daemon started and that the Daemon found while it could not list the processes: `up` answers `reused` and the warning says the Daemon cannot tell that the Service is what answers on its Lease, see [After a crash](daemon.md#after-a-crash). Without `--json` each warning is on stderr as `procly: warning: ...`, with `--json` stderr stays empty and the MCP `up` tool returns the document, so an agent reads them in `warnings` and nowhere else. The key was added without a change of the schema version.

```console
$ procly up --json web
{"schema_version":1,"result":"healthy","services":[{"service":"api","outcome":"started","via":null,"port":4100,"url":"http://localhost:4100","ports":{"http":4100},"urls":{"http":"http://localhost:4100"},"needed_by":"web","warnings":[]},{"service":"web","outcome":"started","via":null,"port":4101,"url":"http://localhost:4101","ports":{"http":4101},"urls":{"http":"http://localhost:4101"},"needed_by":null,"warnings":[]}]}
```

`procly up --json` without a Service lists every Service it brought up, all with a `needed_by` of `null`.

Up to v0.1, `url` was an `http` URL of the Lease the health check probes, whatever that port spoke, so a Mailpit whose health check probes its SMTP port had an `http` URL for it. Since v0.2 `url` has the scheme of that port, or is `null` when Procly does not know it. The key keeps its meaning, where to reach the Service, and only a value that did not say it changed, so the schema version stays 1. `urls` is a new key.

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

`service` is the Service asked for, or `null` for all of them. `stopped` also lists the Isolated Compose services whose container of the Worktree was stopped. `released` lists the Shared and Logical Compose services the Worktree let go of, each with `service`, `others` (how many other Worktrees still need the container) and `grace_ms` (how long it keeps running once none does).

## `worktree hooks`

```console
$ procly worktree hooks --json
{"schema_version":1,"file":"/home/me/app/.config/wt.toml","outcome":"written","set":[],"lines":["pre-start = \"procly worktree init\"","pre-remove = \"procly worktree release\""]}
```

`file` is the Worktrunk config of the Worktree. `outcome` is `written`, `unchanged` when the block was in place, `edited` when you edited the block, or moved it under a table header where its keys no longer are the hooks, and Procly left it, or `conflict` when a hook is already set outside the block and nothing was written. `set` names those hooks (`pre-start`, `pre-remove`) for a `conflict` and is empty otherwise. `lines` holds the lines of the block, which the text output prints after a `conflict` as well: the line of a hook in `set` is to wire into that hook, the other is to add as it is. A `conflict` exits with 5, as without `--json`.

## `gc`

```console
$ procly gc --json
{"schema_version":1,"applied":false,"orphans":[{"kind":"lease","service":"web","port_name":"http","port":24081,"repository":"/home/me/app/.git","worktree":"/home/me/app-login","deleted":false,"error":null},{"kind":"logical_resource","service":"db","resource":"dev","name":"app__app_login","driver":"postgres","repository":"/home/me/app/.git","worktree":"/home/me/app-login","deleted":false,"error":null},{"kind":"setup_directory","path":"/home/me/.local/state/procly/setup/app_login-5c2e0d6b8a41f7e3","repository":"/home/me/app/.git","worktree":"/home/me/app-login","deleted":false,"error":null}],"skipped":[],"warnings":[]}
```

`applied` is `true` for `gc --apply`. `orphans` lists every Orphan in the order `--apply` deletes them, the Leases, then the Logical resources, then the output directories, the setup directories, the Isolated containers and their volumes. Each one has a `kind` of `lease` (with `service`, `port_name` and `port`), `logical_resource` (with `service`, `resource`, the key of the resource in the configuration, `name`, its name or index in the container, and `driver`), `output_directory` or `setup_directory` (with `path`), `isolated_container` (with the `service`, its compose `project` and the `compose` service) or `isolated_volume` (with the compose `project` and the `volume`), the `repository` and the removed `worktree` it belongs to, whether it was `deleted`, always `false` without `--apply`, and the `error` that kept it from being deleted, or `null`. `skipped` lists the Repositories whose Worktrees git could not list, each with its `repository` and the `error` of git, nothing of which is an Orphan. `warnings` lists what went wrong without failing `gc`, such as a [Portless alias](configuration.md#portless-aliases) that could not be removed, once for all the Orphans and not once for each, and is empty when nothing did. `worktree release --json` has the same `warnings`. Without `--json` each warning is on stderr as `procly: warning: ...`.

The kinds `isolated_container` and `isolated_volume` were added without a change of the schema version: a reader that does not know a `kind` should skip that Orphan.

`gc --apply --json` exits with 6 when some `error` is not `null`, as without `--json`, and prints none of the lines of stderr, which the document carries.

## `doctor`

```console
$ procly doctor --json
{"schema_version":1,"result":"problems","checks":[{"name":"docker_compose","status":"passed","message":"docker compose 2.29.1","fix":null},{"name":"daemon","status":"passed","message":"the Daemon answers and runs procly 0.4.0","fix":null},{"name":"session_unit","status":"warning","message":"the Session unit /home/me/.config/systemd/user/procly.service is not installed, the Daemon does not start with your session","fix":"procly unit install"},{"name":"state_documents","status":"passed","message":"every state document can be read","fix":null},{"name":"configuration","status":"passed","message":"/home/me/app/procly.toml is valid and its Drivers load","fix":null},{"name":"guard","status":"skipped","message":"/home/me/app/.claude/settings.json holds no Guard, `procly agent install` writes it if you want your agents steered","fix":null},{"name":"portless","status":"skipped","message":"settings.portless is not on, nothing calls portless","fix":null},{"name":"worktrunk_hooks","status":"skipped","message":"/home/me/app/.config/wt.toml holds no Worktrunk hooks, `procly worktree hooks` writes them if you use Worktrunk","fix":null},{"name":"leased_ports","status":"failed","message":"lease web.http port 24081 of /home/me/app-login is held by process 4812 node, which Procly did not start","fix":"kill 4812"},{"name":"orphans","status":"failed","message":"3 Orphans left by removed Worktrees","fix":"procly gc"},{"name":"portless_aliases","status":"passed","message":"every Portless alias Procly made is held by a Lease","fix":null}],"prompt":"`procly doctor` checked this machine and found 2 problems listed below. [...]","agent":null,"daemon_version":"0.4.0"}
```

`result` is `healthy` when no check failed, whatever the `warning` and `skipped` ones, and `problems` otherwise, and `doctor --json` exits with 7 in that case, as without `--json`. `checks` lists every check in the order the text prints them: `docker_compose`, `daemon`, `session_unit`, `state_documents`, `configuration`, `guard`, `portless`, `worktrunk_hooks`, `leased_ports`, `orphans` and `portless_aliases`, see [Checking the installation](doctor.md#the-checks). Each one has a `name`, a `status` of `passed`, `failed`, `skipped` or `warning`, a `message` and a `fix`. A `warning` is something missing that Procly can do without, such as a Session unit that is not installed: it carries its `fix` like a `failed` check and changes neither the `result` nor the exit code. A reader that does not know a `status` should treat it as no problem. The `message` of a check with several problems holds one per line. The `fix` is the command that fixes the problem, or `null` when nothing is wrong or no command fixes it, and holds one command per line when there are several. Doctor runs none of them.

`prompt` is what `procly doctor --agent` would hand to a coding agent for the failed checks, so that your own agent can act on it, and `null` when no check failed. With `--agent <cli>` the prompt is handed to that CLI, `prompt` is `null` and `agent` names the CLI, `null` otherwise. What the agent prints on its standard output goes to stderr. See [Checking the installation](doctor.md#handing-the-problems-to-your-coding-agent).

`daemon_version` is the version of procly the running Daemon runs, the one the `daemon` check compares with this procly, so that a reader does not have to take it from the `message`. It is `null` when no Daemon answers the checks: none runs, it fails, or it speaks another protocol version.

A later version of procly may add a check. A reader that does not know a `name` should skip that check, and the schema version stays 1.

## `restart`

`procly restart --json <service>` prints the document of `procly up --json <service>`, with the same `result` values and exit codes. The Service restarted has an `outcome` of `started`, a Service it needs that was running is `reused`, and one that was not is `started`. A Service a Group covers is restarted through its Group, which is then the Service of the document, whether the Group runs or the Service ended, except a covered Service that runs on its own, which its Group cannot start over and which is restarted alone. A Compose service follows the path of `down` then `up`: the container of an Isolated one is stopped and started again, while a Shared or Logical one is let go of and reached again, so its container, which other Worktrees may use, keeps running and is `reused`. A Worktree release of the same Worktree that begins during a Restart wins, the Restart then exits with 125, and a release that begins while the Restart is starting a Compose service waits for that start to finish, see [Releasing a Worktree](worktree-release.md#what-it-does).

```console
$ procly restart --json web
{"schema_version":1,"result":"healthy","services":[{"service":"web","outcome":"started","via":null,"port":4101,"url":"http://localhost:4101","ports":{"http":4101},"urls":{"http":"http://localhost:4101"},"needed_by":null,"warnings":[]}]}
```

## `daemon stop`

```console
$ procly daemon stop --json
{"schema_version":1,"outcome":"stopped","pid":4242,"waited_ms":5310}
$ procly daemon stop --json
{"schema_version":1,"outcome":"not_running","pid":null}
```

`outcome` is `stopped` when a Daemon ran and has gone, with the `pid` it was sent `SIGTERM` and `waited_ms`, the time it took to go in milliseconds. It is `not_running` when no Daemon held the lock of the state directory, a pid file left by a crash included, and `pid` is then `null` and there is no `waited_ms`. A Daemon still there after 10 s prints no document: the command exits with 125 and the reason on stderr, see [exit codes](exit-codes.md). The command never speaks the protocol, so the document is the same whatever version of Procly the Daemon runs, see [Stopping the Daemon](daemon.md#stopping-the-daemon).

## `status`

```console
$ procly status --json
{"schema_version":1,"repositories":[{"git_common_dir":"/home/me/app/.git","worktrees":[{"path":"/home/me/app","slug":"app","main":true,"branch":"main","listed":true,"services":[{"service":"web","port":4101,"reattached":false,"started_at":1793900000123,"restartable":true,"state":"healthy"}],"jobs":[{"id":"3f9a1c2e","command":"bun test","cwd":"/home/me/app","state":"ended","end":{"kind":"exited","code":3}}]}]}],"jobs_outside_repositories":[]}
```

`services` lists the Process services and Groups Procly started in the Worktree. A Group stands for the Services it covers: they have no row of their own while it runs, and the ended rows they had are forgotten when the Group starts, see [Groups](configuration.md#groups). A Service has a `state` of `starting`, `healthy`, `restarting` or `ended`, and an ended one carries an `end` like the one of `up`. A Shared or Logical Compose service is not listed, in the text nor in the document: its container serves every Worktree of the Repository, `procly up <service>` answers `reused` while it runs, `procly down` tells how many Worktrees still need it, and `procly logs <service>` prints its recent output. A Worktree carries the `branch` checked out in it, as the short name git lists it under, and `listed`, whether `git worktree list` still lists it. `branch` is left out on a detached `HEAD`, and for a Worktree git no longer lists, which has `listed` set to `false` and shows as `(removed)` in the text. A Service carries `started_at`, the time it last started in milliseconds since the Unix epoch, set again by each automatic restart. For a Service a previous Daemon started, it is the time the Daemon found it running. The 3 keys were added without a change of the schema version. A Service also carries `restartable`, whether the Daemon holds the environment of its last `up`, which `procly restart` replays. It is `false` for a Service a previous Daemon started and after a Worktree release, and the environment itself is never part of the status. The key was added without a change of the schema version. In the text, `procly status` prints `  worktree app (main) /home/me/app on main`.

A Worktree lists its Foreign processes under `foreign` when it has some, see [Foreign processes](daemon.md#foreign-processes).

A Worktree lists the Isolated containers Procly runs for it under `containers` when it has some, each with `service`, the compose `project` of the Worktree, the `compose` service and the `port` its health check probes, see [Isolated mode](configuration.md#isolated-mode). The key was added without a change of the schema version, and is left out when the list is empty:

```json
"containers":[{"service":"cache","project":"app-app_login","compose":"redis","port":24001}]
```
