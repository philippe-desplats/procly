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

Before the Worktree is removed, `procly worktree release` stops its work and releases its Leases, see [Releasing a Worktree](worktree-release.md).

## Worktrunk

Worktrunk runs the hooks of its project config, `.config/wt.toml`, when it creates and removes a Worktree. `procly worktree hooks` writes the 2 hooks that call Procly, so that Worktrunk drives it with no script of yours:

```console
$ procly worktree hooks
written /home/me/app/.config/wt.toml
$ cat .config/wt.toml
# >>> procly managed block: worktrunk >>>
pre-start = "procly worktree init"
pre-remove = "procly worktree release"
# <<< procly managed block: worktrunk <<<
```

`pre-start` prepares the new Worktree with `worktree init`, and `pre-remove` runs `worktree release` before Worktrunk removes it. The command runs in the Worktree you are in, creates the file when it is missing, and is idempotent: a second run prints `unchanged` and leaves one block.

The block is a Managed block (ADR 0008), delimited by TOML comment markers and recorded with the fingerprint of its content. Procly puts it at the top of the file, since a key written after a table header such as `[post-start]` would belong to that table. The rest of the file stays as it was, byte for byte.

- Procly never overwrites a hook of yours. When `pre-start` or `pre-remove` is already set outside the block, as a key or as a table, it writes nothing, prints on stdout the lines of both hooks and exits with 5, see [exit codes](exit-codes.md). Add the Procly command to the hook that is set, or replace it, and add the line of the hook that is not set as it is: stderr says which one it is, and without it `worktree release` never runs on removal.
- A block you edited, or moved under a table header where its keys belong to that table and hook nothing, is left as it is, the command prints `edited` on stdout and a note on stderr, and exits with 0.
- A `.config/wt.toml` that is not valid TOML is not touched, and the command exits with 125.
- `procly eject` takes an intact block out and gives the file back as it was, and deletes the file and its directory when only the block created them. A block you edited stays and is described in the prompt, see [Removing Procly from a Repository](eject.md).
- `procly init` suggests the command on stderr when the Worktree has a `.config/wt.toml` without the block, and writes nothing into it.
- `procly doctor` reports a block that is not the one Procly recorded, see [Checking the installation](doctor.md).

Worktrunk asks you to approve the project hooks the first time it runs them, and Procly does not bypass that approval: answer it in Worktrunk. `--json` prints `file`, the `outcome` (`written`, `unchanged`, `edited` or `conflict`), the hooks already `set` outside the block for a conflict, and the `lines` of the block, see [JSON output](json.md#worktree-hooks). `worktree hooks` writes into your Repository, so the MCP server does not offer it, see [The MCP server](mcp.md).
