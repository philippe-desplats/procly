# Collecting Orphans

A Worktree removed with `git worktree remove`, or by a worktree tool, leaves behind what Procly held for it. These leftovers are Orphans: something whose Worktree git no longer lists, and that no Pinned branch holds. `procly gc` lists the Orphans of every Repository the Daemon knows, and deletes nothing. `procly gc --apply` deletes them.

```console
$ procly gc
lease web.http port 24081 of /home/me/app-login
logical resource db.dev app__app_login (driver postgres) of /home/me/app-login
logical resource cache.main 2 (driver redis) of /home/me/app-login
output directory /home/me/.local/state/procly/output/app_login-5c2e0d6b8a41f7e3 of /home/me/app-login
setup directory /home/me/.local/state/procly/setup/app_login-5c2e0d6b8a41f7e3 of /home/me/app-login
procly: 5 Orphans, `procly gc --apply` deletes them
$ procly gc --apply
deleted lease web.http port 24081 of /home/me/app-login
deleted logical resource db.dev app__app_login (driver postgres) of /home/me/app-login
deleted logical resource cache.main 2 (driver redis) of /home/me/app-login
deleted output directory /home/me/.local/state/procly/output/app_login-5c2e0d6b8a41f7e3 of /home/me/app-login
deleted setup directory /home/me/.local/state/procly/setup/app_login-5c2e0d6b8a41f7e3 of /home/me/app-login
$ procly gc
no Orphans
```

## What is an Orphan

`gc` collects 4 kinds of Orphans:

- A Lease of a removed Worktree. Deleting it returns its port to the Range at once, instead of after `settings.lease_expiry`.
- A Logical resource of a removed Worktree, such as its Postgres database or its Redis database index, listed with its Service, its key in the configuration, its name in the container and its Driver. Deleting it drops it in its container through the `drop` command of its Driver, then frees the index it held when the Driver numbers its resources, so that a new Worktree can be granted that index. A Redis container and its 16 indexes never run out because of removed Worktrees.
- The output directory of a removed Worktree, in the `output` directory of the state directory, which held the output files of its Services.
- The setup directory of a removed Worktree, in the `setup` directory of the state directory, which holds the log of a setup command that failed, see [Preparing a Worktree](worktree-init.md).

Each of these directories records the Repository and the Worktree it belongs to. A directory without that record, such as the files written by Procly 0.3 and earlier straight into `output` and `setup`, cannot be placed and is never an Orphan.

Procly records the Logical resources of every Worktree other than the Main worktree in `resources.json` of the state directory, as soon as `procly up` or `procly env` works out their name or index. The resources of the Main worktree never become Orphans, nor the ones of a Shared or Isolated Service, which use the names of the Main worktree. A resource worked out by Procly 0.3 and earlier has no record and is never an Orphan, drop it by hand. A Worktree that still exists records its resources again on its next `procly up`.

A Worktree counts as removed only once `git worktree list` no longer lists it. Procly never guesses it from a missing directory:

- A Worktree git still lists is never removed, even one marked prunable because its directory is gone, as on a disk that is not mounted. `git worktree prune` makes git forget it, and its leftovers become Orphans.
- The Leases of a Pinned branch are bound to the branch, not to a Worktree, so that the branch gets the same ports when its Worktree is recreated. They are never Orphans. Nor are the Logical resources of a Worktree that was on a Pinned branch, as long as the `procly.toml` of the Main worktree pins it, or cannot be read.
- A Driver that names its resources names them after the Worktree slug, which only depends on the name of the Worktree directory. A named resource of a removed Worktree is never an Orphan while git lists a Worktree whose directory has the same name, such as `/work/a/login` removed and `/work/b/login` created, since that Worktree uses the same resource.
- Nothing of a Worktree where a Service still runs, or where `worktree init` still runs a setup command, is an Orphan until it ends.
- A Repository whose Worktrees git cannot list, because its git directory was moved or deleted, has no Orphans. `gc` says so on stderr, `procly: cannot list the Worktrees of <repository>, nothing of it is an Orphan`, with the error of git.

## Deleting

Procly never deletes on its own inference (ADR 0004): only `procly gc --apply` deletes Orphans. It deletes the Leases first, then the Logical resources, then the output directories, then the setup directories, and prints one `deleted` line per Orphan on stdout. An Orphan it cannot delete is reported on stderr with the reason, and `gc` goes on with the next one:

```console
$ procly gc --apply
deleted lease web.http port 24081 of /home/me/app-login
deleted setup directory /home/me/.local/state/procly/setup/app_login-5c2e0d6b8a41f7e3 of /home/me/app-login
procly: cannot delete output directory /home/me/.local/state/procly/output/app_login-5c2e0d6b8a41f7e3 of /home/me/app-login: cannot remove /home/me/.local/state/procly/output/app_login-5c2e0d6b8a41f7e3: Permission denied (os error 13)
procly: 1 of 3 Orphans could not be deleted
$ echo $?
6
```

`gc --apply` then exits with 6, see [exit codes](exit-codes.md). The Orphans that failed stay Orphans, and the next `gc --apply` tries them again.

A Logical resource is dropped in the container its Worktrees shared, which must run: `gc` never starts a container. When the container does not run, the resource is reported as not deleted, and `procly up <service>` in the Main worktree starts it for the next `gc --apply`. The Driver is read from the Main worktree, and each of its commands may run for `settings.driver_timeout` of the Main worktree. When the Driver has a `ready` command, `gc` runs it first, then `check`: a resource `check` no longer finds counts as dropped, and is only forgotten. When `drop` fails, the error carries what it printed and the command to drop the resource by hand:

```console
$ procly gc --apply
procly: cannot delete logical resource db.dev app__app_login (driver postgres) of /home/me/app-login: the postgres Driver could not drop it: exit status: 1: dropdb: error: database "app__app_login" is being accessed by other users, drop it by hand with `docker compose --project-name app exec -T db sh -c 'dropdb --username "${POSTGRES_USER:-postgres}" --if-exists app__app_login'`
procly: 1 of 1 Orphan could not be deleted
```

The record of a resource is forgotten and its index freed only once `drop` succeeded, so that an index is never granted to a new Worktree while it still holds the data of a removed one.

`gc --apply` holds the container through the check, the drop and the forgetting, as `procly up` does to create resources in it, and works out once more, with the container held, whether the resource is still an Orphan. A Worktree added while `gc --apply` runs, and brought up meanwhile, thus keeps the resources it uses: such a resource is no longer an Orphan, and is neither dropped nor reported.

`procly env`, and the `procly up` of a Service process, are handed their indexes without the container, so `gc --apply` works out once more, after the drop and under the lock that hands the indexes out, whether the resource is still an Orphan before it forgets it and frees its index. A Worktree added again at the same path while its resource is dropped thus keeps its record and its index, and the resource is not reported: its data was the one of the removed Worktree, and its next `procly up` creates it again.

The Daemon log names who asked and every deletion, `gc deleted lease web.http port 24081 of /home/me/app-login, requested by pid 4242 (procly) in /home/me/app, parent pid 4100 (zsh)`, and every failure, so that a deletion can always be traced.

## JSON

With `--json`, `gc` and `gc --apply` print 1 document, see [JSON output](json.md#gc).
