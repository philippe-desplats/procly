# Collecting Orphans

A Worktree removed with `git worktree remove`, or by a worktree tool, leaves behind what Procly held for it. These leftovers are Orphans: something whose Worktree git no longer lists, and that no Pinned branch holds. `procly gc` lists the Orphans of every Repository the Daemon knows, and deletes nothing. `procly gc --apply` deletes them.

```console
$ procly gc
lease web.http port 24081 of /home/me/app-login
output directory /home/me/.local/state/procly/output/app_login-5c2e0d6b8a41f7e3 of /home/me/app-login
setup directory /home/me/.local/state/procly/setup/app_login-5c2e0d6b8a41f7e3 of /home/me/app-login
procly: 3 Orphans, `procly gc --apply` deletes them
$ procly gc --apply
deleted lease web.http port 24081 of /home/me/app-login
deleted output directory /home/me/.local/state/procly/output/app_login-5c2e0d6b8a41f7e3 of /home/me/app-login
deleted setup directory /home/me/.local/state/procly/setup/app_login-5c2e0d6b8a41f7e3 of /home/me/app-login
$ procly gc
no Orphans
```

## What is an Orphan

`gc` collects 3 kinds of Orphans:

- A Lease of a removed Worktree. Deleting it returns its port to the Range at once, instead of after `settings.lease_expiry`.
- The output directory of a removed Worktree, in the `output` directory of the state directory, which held the output files of its Services.
- The setup directory of a removed Worktree, in the `setup` directory of the state directory, which holds the log of a setup command that failed, see [Preparing a Worktree](worktree-init.md).

Each of these directories records the Repository and the Worktree it belongs to. A directory without that record, such as the files written by Procly 0.3 and earlier straight into `output` and `setup`, cannot be placed and is never an Orphan.

A Worktree counts as removed only once `git worktree list` no longer lists it. Procly never guesses it from a missing directory:

- A Worktree git still lists is never removed, even one marked prunable because its directory is gone, as on a disk that is not mounted. `git worktree prune` makes git forget it, and its leftovers become Orphans.
- The Leases of a Pinned branch are bound to the branch, not to a Worktree, so that the branch gets the same ports when its Worktree is recreated. They are never Orphans.
- Nothing of a Worktree where a Service still runs, or where `worktree init` still runs a setup command, is an Orphan until it ends.
- A Repository whose Worktrees git cannot list, because its git directory was moved or deleted, has no Orphans. `gc` says so on stderr, `procly: cannot list the Worktrees of <repository>, nothing of it is an Orphan`, with the error of git.

## Deleting

Procly never deletes on its own inference (ADR 0004): only `procly gc --apply` deletes Orphans. It deletes the Leases first, then the output directories, then the setup directories, and prints one `deleted` line per Orphan on stdout. An Orphan it cannot delete is reported on stderr with the reason, and `gc` goes on with the next one:

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

The Daemon log names who asked and every deletion, `gc deleted lease web.http port 24081 of /home/me/app-login, requested by pid 4242 (procly) in /home/me/app, parent pid 4100 (zsh)`, and every failure, so that a deletion can always be traced.

## JSON

With `--json`, `gc` and `gc --apply` print 1 document, see [JSON output](json.md#gc).
