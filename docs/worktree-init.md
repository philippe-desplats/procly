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

With `--json`, stdout holds 1 document with a `result` of `ready` and the lists `copied`, `missing` and `ran`. A setup command that fails gives a `result` of `setup_failed` instead, with the same lists up to it, the `command`, how it ended in `end` and the end of its `output`, and the exit code stays the one of the command. See [JSON output](json.md).

```console
$ procly worktree init --json
{"schema_version":1,"result":"ready","copied":[".env"],"missing":[],"ran":["echo ready"]}
$ procly worktree init --json
{"schema_version":1,"result":"setup_failed","copied":[],"missing":[],"ran":[],"command":"echo boom; exit 7","end":{"kind":"exited","code":7},"output":"boom\n"}
```

## What it does

1. It copies each `copy` entry from the Main worktree to the same path in the Worktree. A directory is copied file by file, and whatever the Worktree already holds is left alone, so a file you edited is never overwritten. An entry the Main worktree does not hold is reported on stderr and skipped.
2. It runs each `setup` command that has not succeeded in this Worktree yet, in order, and stops at the first one that fails. The command runs with the environment of the `procly worktree init` that asked for it, in its own process group, and its tree is killed once it has run for `settings.setup_timeout`, 30 minutes by default, see [Configuration](configuration.md#settings).

A command counts as done once it exits with 0, and it is identified by its text. A second `worktree init` therefore changes nothing, a command added to the list later runs alone, and a command that failed runs again. The `worktree init` exits with the code of the failing command, see [exit codes](exit-codes.md), and prints the end of its output on stderr.

The record of the commands that succeeded lives in the git directory of the Worktree, so removing the Worktree removes it, and a Worktree created again at the same path starts afresh. Delete `procly-worktree-init.json` in that directory to run every setup command again. It is a versioned state document like the ones of the state directory (ADR 0012), so a record written by a newer Procly is refused rather than misread.

Run in the Main worktree, `worktree init` has nothing to copy and only runs the setup commands.

## A long setup

A setup command that is still running after `settings.setup_progress_interval`, 1 minute by default, prints a line on stderr, then another one each interval until it ends, so that a setup waiting on a slow package registry shows it is alive and where to look:

```console
$ procly worktree init
procly: setup command `pnpm install` still running after 1m, log: /home/me/.local/state/procly/setup/app_feature-5c2e0d6b8a41f7e3/setup.log
procly: setup command `pnpm install` still running after 2m, log: /home/me/.local/state/procly/setup/app_feature-5c2e0d6b8a41f7e3/setup.log
ran pnpm install
```

The log holds the output of the setup commands of that Worktree, each one after a `$ command` line, and `tail -f` follows it while they run. It sits in the setup directory of the Worktree, in the `setup` directory of the state directory. It is emptied by the next `worktree init` of the Worktree, removed with its directory once every setup command succeeded, and kept when one failed or ran past `settings.setup_timeout`, so that the path of the last line stays valid to tell why. Once git no longer lists the Worktree, its setup directory is an Orphan that `procly gc --apply` deletes, see [Collecting Orphans](gc.md). The exit code and the message of a setup command that ran past its time limit are the same with or without these lines.

With `--json`, these lines are still on stderr and stdout holds its single document.

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
