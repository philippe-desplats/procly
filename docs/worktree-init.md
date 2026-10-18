# Preparing a Worktree

`procly worktree init` makes a fresh Worktree ready with one command. Procly never creates Worktrees (ADR 0011), so run it after `git worktree add`, Worktrunk, Orca or whatever created the Worktree.

```toml
[worktree]
copy = [".env", "certs"]
setup = ["pnpm install", "pnpm db:generate"]
```

```console
$ cd ../app-feature
$ procly worktree init
copied .env
copied certs
ran pnpm install
ran pnpm db:generate
$ procly worktree init
nothing to do
```

## What it does

1. It copies each `copy` entry from the Main worktree to the same path in the Worktree. A directory is copied file by file, and whatever the Worktree already holds is left alone, so a file you edited is never overwritten. An entry the Main worktree does not hold is reported on stderr and skipped.
2. It runs each `setup` command that has not succeeded in this Worktree yet, in order, and stops at the first one that fails. The command runs with the environment of the `procly worktree init` that asked for it, in its own process group, and its tree is killed after 30 minutes.

A command counts as done once it exits with 0, and it is identified by its text. A second `worktree init` therefore changes nothing, a command added to the list later runs alone, and a command that failed runs again. The `worktree init` exits with the code of the failing command, see [exit codes](exit-codes.md), and prints the end of its output on stderr.

The record of the commands that succeeded lives in the git directory of the Worktree, so removing the Worktree removes it, and a Worktree created again at the same path starts afresh. Delete `procly-worktree-init.json` in that directory to run every setup command again.

Run in the Main worktree, `worktree init` has nothing to copy and only runs the setup commands.

## Naming the Worktree

`procly worktree init` prepares the Worktree holding the current directory. Pass a path to prepare another one, or any directory inside it:

```console
$ procly worktree init ../app-feature
```

## Replacing an existing script

Orchestrators and worktree tools that call a setup script keep working when the script becomes one line. Give it the path when the caller passes one, and nothing when it runs inside the Worktree:

```sh
#!/bin/sh
exec procly worktree init "$@"
```

Called as `scripts/setup-worktree.sh /path/to/worktree` from anywhere, or as `scripts/setup-worktree.sh` from inside the Worktree, it prepares the right one. Move what the old script did into `copy` and `setup`.
