# Releasing a Worktree

`procly worktree release` runs the Worktree release: it undoes what a Worktree holds in Procly, so that the Worktree can be removed without leaving a process, a container count or a port behind. Procly never removes Worktrees (ADR 0011), so run it before `git worktree remove`, from the hook that Worktrunk, Orca or whatever removes the Worktree runs first.

```console
$ cd ../app-feature
$ procly worktree release
web stopped
job 3f9a1c2e stopped
db released, 1 other Worktree still needs it
lease web.http port 24001 released
Logical resource app__app_feature of db (postgres) kept, `procly gc` lists it once the Worktree is removed
$ procly worktree release
nothing to release
Logical resource app__app_feature of db (postgres) kept, `procly gc` lists it once the Worktree is removed
```

## What it does

1. It stops every Service and every running Job of the Worktree with their whole process trees, `SIGTERM` to each process group, then `SIGKILL` once the stop timeout has passed.
2. It lets go of the Compose services of the Worktree as `procly down` does: a Shared or Logical container keeps running while another Worktree needs it, then stops after its Grace period. The Isolated container of the Worktree stops at once and is printed as `<service> stopped`, kept with its volumes, see [Isolated mode](configuration.md#isolated-mode).
3. It releases the Leases of the Worktree, so that the next Worktree can be granted their ports at once, without waiting for them to expire. With `portless = true`, it removes their [Portless alias](configuration.md#portless-aliases) too.

It also applies the [Retention policy](configuration.md#retention-policy) of the Isolated containers of the Worktrees already removed, of every Repository: those set to `"delete"` are removed, and those set to a duration are removed once it has passed. The Worktree being released still exists, so its own container is kept until a later release, or the next start of the Daemon, finds it removed.

It keeps the rest, and deletes no data (ADR 0004):

- The Leases bound to a Pinned branch, so that the branch gets the same ports when its Worktree is recreated. Each one is printed as `kept for Pinned branch <branch>`.
- The Leases a container still runs on, which are the Leases of the Main worktree: run in the Main worktree, the release keeps them while the container runs out its Grace period. Each one is printed as `kept, its container still runs`.
- Every Logical resource the configuration gives the Worktree, such as its Postgres databases, and for a Driver that numbers its resources, such as Redis, the indexes the Worktree holds. Each one is printed with the `procly gc` command, which lists it as an Orphan once git no longer lists the Worktree.
- The Docker volumes, the files `worktree init` copied and the Env file.

The Daemon logs who asked for the release, with what it stopped and released, as it does for `procly down`. Running it again, or in a Worktree Procly never saw, prints `nothing to release` and exits with 0, see [exit codes](exit-codes.md).

`procly worktree release` releases the Worktree holding the current directory. Pass a path to release another one, or any directory inside it:

```console
$ procly worktree release ../app-feature
```

## JSON

With `--json`, stdout holds 1 document with the `worktree`, then what was `stopped` (`services` by name, the Isolated Compose services included, and `jobs` by identifier), what was `released` (the `compose_services` let go of, each with `service`, `others` and `grace_ms` as for [`down`](json.md#down), and the `leases`, each with `service`, `port_name` and `port`) and what was `kept`. A kept Lease carries a `reason` of `pinned_branch`, with its `branch`, or `container_running`. A kept Logical resource carries its `service`, its `driver`, the key of the `resource` in the configuration and its `name`. The schema version is 1, see [JSON output](json.md).

```console
$ procly worktree release --json
{"schema_version":1,"worktree":"/home/me/app-feature","stopped":{"services":["web"],"jobs":["3f9a1c2e"]},"released":{"compose_services":[{"service":"db","others":1,"grace_ms":600000}],"leases":[{"service":"web","port_name":"http","port":24001}]},"kept":{"leases":[],"logical_resources":[{"service":"db","driver":"postgres","resource":"dev","name":"app__app_feature"}]}}
```

## Calling it from a worktree manager

Run it where the Worktree still exists, before it is removed, from the hook your tool runs before it removes a Worktree. A script that a tool calls with the path of the Worktree becomes one line:

```sh
#!/bin/sh
exec procly worktree release "$@"
```
