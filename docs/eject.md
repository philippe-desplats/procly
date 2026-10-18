# Removing Procly from a Repository

Procly can always be removed (ADR 0008). `procly eject` stops the Services of the Repository and takes out of it everything Procly wrote, so that its tracked files are what they were before Procly. It never touches Docker data.

```console
$ procly eject
stopped web of /home/me/app
stopped db (container, its volumes are kept)
removed rules /home/me/app/AGENTS.md
removed guard /home/me/app/.claude/settings.json, the file held nothing else and was deleted
removed exclusions /home/me/app/.git/info/exclude
removed env file /home/me/app/.env.procly
left rules /home/me/app/docs/agents.md, edited
left Docker volumes and Logical resources

procly: 1 item was edited, left as it is, the prompt below describes what to undo, `procly eject --agent <cli>` hands it to your coding agent
Procly was ejected from the git repository whose common git directory is /home/me/app/.git. ...
```

Stdout holds the summary, then a blank line and the prompt when something was left. The note about it goes to stderr.

## What it does

It acts on the whole Repository, whatever Worktree it runs in.

1. It stops every Service of every Worktree, with its whole process tree. The containers of Compose services are stopped at once, without waiting for their Grace period, and never removed. If a container cannot be stopped, eject fails before it touches a file, and running it again finishes the job.
2. It removes the Rules blocks, in `AGENTS.md`, `CLAUDE.md` and any file `procly agent install --file` wrote to.
3. It removes the Guard from `.claude/settings.json`. The other hooks and settings stay, in their order, but the file is written again in JSON with 2 spaces of indentation, as `agent install` did.
4. It removes the exclusion block from the shared git exclude file and from the `.gitignore` files that hold it.
5. It removes the Env file, `.env.procly`, of every Worktree.

A file that held nothing else once its block was gone is deleted, since only Procly can have created it, and so are the directories that leaves empty inside the Worktree. Every other byte of a file stays as it was. A file that links out of the Repository is never written, as for `agent install`.

A block counts as intact when its content is the one Procly recorded the fingerprint of, or the one Procly writes today when the record was lost. Anything else is a block you edited. An Env file counts as Procly's while it starts with its header.

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
