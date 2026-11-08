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
3. It forgets every Job and Service of the Worktree that has ended, the ones it has just stopped included, so that `procly wait`, `procly logs` and `procly status` no longer know them.
4. It releases the Leases of the Worktree, so that the next Worktree can be granted their ports at once, without waiting for them to expire. With `portless = true`, it removes their [Portless alias](configuration.md#portless-aliases) too.

It also applies the [Retention policy](configuration.md#retention-policy) of the Isolated containers of the Worktrees already removed, of every Repository: those set to `"delete"` are removed, and those set to a duration are removed once it has passed. The Worktree being released still exists, so its own container is kept until a later release, or the next start of the Daemon, finds it removed. Each policy whose deletion is due is printed with the container it applies to. A duration that has not passed yet is neither applied nor printed, the Daemon waits for it and deletes the container once it has passed:

```console
$ procly worktree release
web stopped
Retention policy delete of db applied to the Isolated container of the removed Worktree /home/me/app-login
```

It keeps the rest, and deletes no data (ADR 0004):

- The Leases bound to a Pinned branch, so that the branch gets the same ports when its Worktree is recreated. Each one is printed as `kept for Pinned branch <branch>`.
- The Leases a container still runs on, which are the Leases of the Main worktree: run in the Main worktree, the release keeps them while the container runs out its Grace period. Each one is printed as `kept, its container still runs`.
- Every Logical resource the configuration gives the Worktree, such as its Postgres databases, and for a Driver that numbers its resources, such as Redis, the indexes the Worktree holds. Each one is printed with the `procly gc` command, which lists it as an Orphan once git no longer lists the Worktree.
- The Docker volumes, the files `worktree init` copied and the Env file.

The Daemon logs who asked for the release, with what it stopped and released, as it does for `procly down`. Running it again, or in a Worktree Procly never saw, prints `nothing to release` and exits with 0, see [exit codes](exit-codes.md). A release that has nothing of its Worktree to stop or release but applies a Retention policy, which may delete an Isolated container and its volumes, does not say `nothing to release`: it prints the Retention line alone, `would apply` in a dry run and `applied` in the release.

A release that meets a `procly restart` in the same Worktree wins: the Restart starts nothing more, takes back no Lease and exits with 125. While that Restart is starting a Compose service, the release waits for the start to finish before it stops anything. The wait lasts as long as the start of the container: up to the health timeout of the service, plus the time its Driver takes to be ready and to apply any Seed of its Logical resources, each Driver command bounded by `driver_timeout`. A Restart of a Process service holds the release only for the milliseconds its spawn takes. A release of another Worktree, and a plain `procly up`, never wait for a Restart.

`procly worktree release` releases the Worktree holding the current directory. Pass a path to release another one, or any directory inside it:

```console
$ procly worktree release ../app-feature
```

## Looking before releasing

`procly worktree release --dry-run` prints what the release would do and changes nothing: no Service or Job is stopped, no Compose service is let go of, no Lease is released, no state file is written, and no Retention policy is applied. The lines say `would stop`, `would release` and `would apply`, the last one for each Retention policy the release would apply to the Isolated container of a Worktree already removed, which may delete its data:

```console
$ procly worktree release --dry-run
web would stop
job 3f9a1c2e would stop
db would release, 1 other Worktree still needs it
lease web.http port 24001 would release
Logical resource app__app_feature of db (postgres) kept, `procly gc` lists it once the Worktree is removed
Retention policy delete of db would apply to the Isolated container of the removed Worktree /home/me/app-login
```

A dry run followed by a real release on the same state gives the same report. The `warnings` of a dry run carry only what keeps Procly from reading the Worktree, such as a configuration it cannot parse, which hides the Logical resources. With `--json` it prints the document below.

## JSON

With `--json`, stdout holds 1 document with the `worktree`, then what was `stopped` (`services` by name, the Isolated Compose services included, and `jobs` by identifier), what was `released` (the `compose_services` let go of, each with `service`, `others` and `grace_ms` as for [`down`](json.md#down), and the `leases`, each with `service`, `port_name` and `port`) and what was `kept`. A kept Lease carries a `reason` of `pinned_branch`, with its `branch`, or `container_running`. A kept Logical resource carries its `service`, its `driver`, the key of the `resource` in the configuration and its `name`. `warnings` lists what went wrong without failing the release, such as a [Portless alias](configuration.md#portless-aliases) that could not be removed, and is empty when nothing did. Without `--json` each warning is on stderr as `procly: warning: ...`. `retention` lists the Retention policies the release applied, or with `--dry-run` would apply, to the Isolated containers of the Worktrees already removed, each with the removed `worktree`, the `service` and the `policy` as `procly.toml` declares it, `delete` or a delay such as `7d`, and is empty when there is none. The schema version is 1, see [JSON output](json.md).

```console
$ procly worktree release --json
{"schema_version":1,"worktree":"/home/me/app-feature","stopped":{"services":["web"],"jobs":["3f9a1c2e"]},"released":{"compose_services":[{"service":"db","others":1,"grace_ms":600000}],"leases":[{"service":"web","port_name":"http","port":24001}]},"kept":{"leases":[],"logical_resources":[{"service":"db","driver":"postgres","resource":"dev","name":"app__app_feature"}]},"warnings":[],"retention":[{"worktree":"/home/me/app-login","service":"db","policy":"delete"}]}
```

## Calling it from a worktree manager

Run it where the Worktree still exists, before it is removed, from the hook your tool runs before it removes a Worktree. A script that a tool calls with the path of the Worktree becomes one line:

```sh
#!/bin/sh
exec procly worktree release "$@"
```
