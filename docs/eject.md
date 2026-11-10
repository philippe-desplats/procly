# Removing Procly from a Repository

Procly can always be removed (ADR 0008). `procly eject` stops the Services, Jobs and Isolated containers of the Worktree it runs in and takes out of that Worktree everything Procly wrote, so that its tracked files are what they were before Procly. It never touches Docker data, and it never touches another Worktree. `procly eject --repository` does the same for every Worktree of the Repository.

```console
$ procly eject
stopped web of /home/me/app-login
stopped job 3f9a1c2e of /home/me/app-login, bun test
removed rules /home/me/app-login/AGENTS.md
removed guard /home/me/app-login/.claude/settings.json, the file held nothing else and was deleted
removed env file /home/me/app-login/.env.procly
left rules /home/me/app-login/docs/agents.md, edited
kept web of /home/me/app, it runs for another Worktree
kept rules /home/me/app/AGENTS.md, written for another Worktree
kept env file /home/me/app/.env.procly, written for another Worktree
kept exclusions /home/me/app/.git/info/exclude, shared with the other Worktrees git lists, `procly eject --repository` removes it
left Docker volumes and Logical resources
left the files copied by worktree init and the state of the Daemon, Leases and off

procly: 1 item was edited, left as it is, the prompt below describes what to undo, `procly eject --agent <cli>` hands it to your coding agent
Procly was ejected from the git repository whose common git directory is /home/me/app/.git. ...
```

Stdout holds the summary, then a blank line and the prompt when something was left. The note about it goes to stderr.

With `--json`, stdout holds 1 document instead, with the Services `stopped`, the Jobs stopped under `jobs`, the files `removed` and `left`, what was `spared`, and the `prompt` when something was left, or `null`. With `--agent`, the prompt is handed to the CLI named in `agent` and `prompt` is `null`, and what the agent prints on its standard output goes to stderr. See [JSON output](json.md).

```console
$ procly eject --json
{"schema_version":1,"stopped":[],"jobs":[],"removed":[{"kind":"rules","file":"/home/me/app/AGENTS.md","deleted":true},{"kind":"guard","file":"/home/me/app/.claude/settings.json","deleted":true}],"left":[],"spared":[],"prompt":null,"agent":null}
```

A Service in `stopped` has the `worktree` it ran for, `null` for a Compose service whose container serves every Worktree. An Isolated Compose service has the Worktree of its container and `"isolated":true`, a key left out for every other Service, and its text line reads `stopped cache of /home/me/app-login (Isolated Compose service, its container and volumes are kept)`. A Job in `jobs` has its `job` identifier, its `command` and the `worktree` it ran in. A `kind` is `rules`, `guard`, `exclusions`, `env_file` or `worktrunk_hooks`, and its text line names it `worktrunk hooks`. The fixed lines about what eject never touches are not in the document.

An item in `spared` has a `kind` of `service` (with its `service` and `worktree`), `compose_service` (with its `service` and `others`, how many other Worktrees still need its container) or `file`. A `file` has the `file`, what was `written` there, with the values of a `kind` above, and `shared`, true when every Worktree of the Repository shares it and false when it belongs to another Worktree. `spared` is empty with `--repository`.

## What it does

Without `--repository`, it acts on the Worktree it runs in, the Main worktree included, and on nothing else.

1. It stops every Service and every Job of the Worktree, with its whole process tree. The Isolated containers of the Worktree are stopped at once, without waiting for their Grace period, and never removed. A Shared or Logical container, which every Worktree can use, is stopped the same way only when no other Worktree still needs it. Otherwise it keeps running, the Worktree no longer counts for it, and it is listed as `kept`. Before it stops anything, eject reads every file of the Worktree it is to change, and fails with 125 when one cannot be read, so that nothing is half done. If a container cannot be stopped, it fails before it touches a file, and running it again finishes the job.
2. It removes the Rules blocks of the Worktree, in `AGENTS.md`, `CLAUDE.md` and any file `procly agent install --file` wrote to.
3. It removes the Guard from the `.claude/settings.json` of the Worktree. The other hooks and settings stay, in their order, but the file is written again in JSON with 2 spaces of indentation, as `agent install` did.
4. It removes the exclusion block from the `.gitignore` of the Worktree when it holds it. The one in the shared git exclude file goes only when the Worktree is the only one of the Repository, see below.
5. It removes the Env file, `.env.procly`, of the Worktree.
6. It removes the Worktrunk hooks block that `procly worktree hooks` wrote into the `.config/wt.toml` of the Worktree, see [Worktrunk](worktree-init.md#worktrunk). The rest of the file stays as it was, and a file and a directory that only the block created are deleted.

A file that held nothing else once its block was gone is deleted, since only Procly can have created it, and so are the directories that leaves empty inside the Worktree. Every other byte of a file stays as it was. A file that links out of the Repository is never written, as for `agent install`.

A block counts as intact when its content is the one Procly recorded the fingerprint of, or the one Procly writes today when the record was lost. Anything else is a block you edited. An Env file counts as Procly's while it starts with its header and holds what Procly last wrote, so a line you added to it is an edit.

Procly records how each file looked just before it first wrote into it, so that eject gives it back to the byte when nothing else in it changed: a file that did not end with a line break ends without one again, an instruction file, `.gitignore` or settings file that existed empty stays an empty file, and a settings file keeps the layout and indentation it had. The settings file is kept whole in the state directory for that. When you changed the file around the block, eject removes the block and keeps your changes, and the settings file is written again with 2 spaces of indentation. A block recorded by a Procly that did not keep this, or after the state directory was lost, is removed with the text around it as it is.

## What it keeps for the other Worktrees

A Repository can have many Worktrees, used by other people or by other agents, so that a plain `eject` never reaches past its own. Each thing it keeps is printed as a `kept` line with the reason.

- A Service that runs for another Worktree stays running. Its Env file, Rules block, Guard and Worktrunk hooks stay in place.
- The exclusion block of the shared git exclude file, `.git/info/exclude`, stays as long as git lists another Worktree, since it keeps the generated files of every Worktree out of git, and its `kept` line names `procly eject --repository` as the way to remove it. It goes when the Worktree is the only one the Repository has.
- A file of another Worktree that eject cannot read, such as a settings file that is not JSON, is not its concern. It never fails the eject, and is not listed.
- A Shared or Logical container that another Worktree still needs keeps running.

To take Procly out of the whole Repository, run `procly eject --repository` from any of its Worktrees.

## Ejecting the whole Repository

`procly eject --repository` is the former behaviour of `eject`. From any Worktree, it stops every Service and Job of every Worktree of the Repository, whoever uses them, and the containers of Compose services, the Isolated container of each Worktree included, at once and without waiting for their Grace period. Before it stops anything, it prints a `will stop web of <worktree>` line for each Service of another Worktree that it is about to stop, and a `will stop job <id> of <worktree>, <command>` line for each Job, so that you see what it reaches before it does. With `--json` nothing is printed before the document, which lists the same in `stopped` and `jobs`. Each one is then printed as `stopped web of <worktree>`. It then removes the Rules blocks, the Guards, the exclusion blocks, the Env files and the Worktrunk hooks blocks of every Worktree, and the exclusion block of the shared git exclude file. Nothing is `kept`.

## What it leaves

- The Managed blocks you edited, a Guard group you changed, and an Env file that lost its header. Each one is listed as `left` and described in the prompt.
- Docker volumes and Logical resources, and the containers themselves, which are stopped and not removed.
- The files `worktree init` copied, the record of the setup commands it ran in the git directory of each Worktree, and the state of the Daemon, such as the Leases and `procly off`. `procly off` stays on, so that a Guard you edited does not start blocking again.
- `procly.toml` and `procly.local.toml`, which are yours.

## The prompt

When something was left, eject prints a prompt for a coding agent. For each item it names the file, the markers or the keys to look for, and what Procly wrote, and it asks the agent to undo that and keep everything that is yours. It tells the agent not to run any `procly` command.

`procly eject --agent <cli>` hands that prompt to your own coding agent instead of printing it, from the current directory. Nothing is ever run without the flag. The agent runs in its non-interactive mode, so it applies your own permission settings for edits.

| `--agent` | Command run |
| --- | --- |
| `claude` | `claude -p <prompt>` |
| `codex` | `codex exec <prompt>` |
| `gemini` | `gemini -p <prompt>` |
| `opencode` | `opencode run <prompt>` |

When nothing was left there is no prompt, and the agent is not run. When the agent is missing or exits with another code than 0, eject exits with 125. What it removed stays removed.

## Exit codes

`eject` exits with 0, also when it left blocks because you edited them, and with 125 when Procly itself fails, see [exit codes](exit-codes.md).
