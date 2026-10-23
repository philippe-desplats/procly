# Checking the installation

`procly doctor` checks that Procly can work here: the machine, the Daemon, the state it keeps, the `procly.toml` of the current Worktree and what holds the ports Procly leased. It lists every check with its result, and for each problem the command that fixes it. It changes nothing and runs none of those commands, you decide what runs.

```console
$ procly doctor
passed  docker_compose: docker compose 2.29.1
passed  daemon: the Daemon answers and runs procly 0.4.0
warning session_unit: the Session unit /home/me/.config/systemd/user/procly.service is not installed, the Daemon does not start with your session
        fix: procly unit install
passed  state_documents: every state document can be read
passed  configuration: /home/me/app/procly.toml is valid and its Drivers load
skipped guard: /home/me/app/.claude/settings.json holds no Guard, `procly agent install` writes it if you want your agents steered
skipped portless: settings.portless is not on, nothing calls portless
skipped worktrunk_hooks: /home/me/app/.config/wt.toml holds no Worktrunk hooks, `procly worktree hooks` writes them if you use Worktrunk
failed  leased_ports: lease web.http port 24081 of /home/me/app-login is held by process 4812 node, which Procly did not start
        fix: kill 4812
failed  orphans: 3 Orphans left by removed Worktrees
        fix: procly gc
passed  portless_aliases: every Portless alias Procly made is held by a Lease
procly: 2 checks failed, the commands above fix them and doctor ran none
$ echo $?
7
```

A check is `passed`, `failed`, `skipped` when it does not apply here or could not run, such as the Guard in a directory that is in no git Repository, or `warning` when something is missing that Procly can do without, such as the Session unit. A `warning` names its fix like a `failed` check does, but it is no problem: only a `failed` check is. Several problems of one check are on lines of their own, with one command per line under `fix`.

Doctor works without the Daemon. It runs the checks that need only the machine, the state directory and the Worktree itself, and asks the Daemon for the others. A Daemon that does not run, or does not answer, is a problem of its own, and the 3 checks that need it are `skipped`. Doctor never starts a Daemon, since starting one reconciles what the previous Daemon left and a command that changes nothing cannot do that. Any other command starts it, `procly status` for one.

## The checks

| Check | Passes when | The fix it names |
| --- | --- | --- |
| `docker_compose` | `docker compose version` answers with a major version of 2 or more, which the Compose services need. | On macOS `brew install --cask docker`, nothing elsewhere since the way to install it depends on the system. |
| `daemon` | A Daemon runs, it answers and runs the same version of procly as this command. A Daemon left running by an older procly, or speaking another protocol version, fails. | `kill "$(cat <state directory>/daemon.pid)"`, which stops the Daemon with its Services, so that the next command starts the new one. `procly status` when none runs, which starts one. `tail -n 20 <state directory>/daemon.log` for a Daemon that does not answer. |
| `session_unit` | The [Session unit](daemon.md) is installed and the session manager has loaded it, so that the Daemon comes back with your session. The session manager is asked only when the unit file exists. A unit that is not installed is a `warning`, since the Session unit is optional and any command starts the Daemon. A unit that is installed but not loaded fails, since the file says you want the Daemon to come back and the session manager does not bring it. | `procly unit install` |
| `state_documents` | Every document of the state directory can be read: the Leases, the indexes, the Logical resources, the Services, the containers, the Isolated containers, the Repositories that are Off, the Managed blocks and the Portless aliases. A document that is absent is fine. | `mv <document> <document>.broken` for a document that is corrupt. None for a document written by a newer procly, since moving it aside would lose what that one recorded. |
| `configuration` | The `procly.toml` of the current Worktree, with the user settings and `procly.local.toml`, is valid, and every Driver a Service names loads. `skipped` without a `procly.toml`. | `${EDITOR:-vi}` on the file, or on the Driver file in `.procly/drivers`. |
| `guard` | The Guard in `.claude/settings.json` of the current Worktree is the one `procly agent install` recorded. `skipped` when there is none, since the Guard is opt-in. A Guard that left the settings fails, and so does one you edited. | `procly agent install` when the Guard is gone. None when you edited it, since `agent install` leaves an edited Guard as it is, restore the hook group running `procly guard` by hand. |
| `portless` | `settings.portless` is on and a `portless` executable is on the `PATH` of this command. `skipped` when the setting is off, which it is by default, and when the configuration cannot be read. It is a problem when the setting is on and `portless` is missing, since `procly up` then makes no [Portless alias](configuration.md#portless-aliases). | `npm install --global portless` |
| `worktrunk_hooks` | The Worktrunk hooks block in `.config/wt.toml` of the current Worktree is the one `procly worktree hooks` recorded, see [Worktrunk](worktree-init.md#worktrunk). `skipped` when the file holds no block, since the hooks are opt-in. A block that left the file fails, and so does one you edited or moved under a table header, where its keys belong to that table and hook nothing. | `procly worktree hooks` when the block is gone. None when you edited or moved it, since `worktree hooks` leaves such a block as it is, restore the lines between its markers by hand, at the root of the file. |
| `leased_ports` | No Service ended on a [Port mismatch](exit-codes.md), and nothing Procly did not start answers on a port a Lease holds while its Service does not run. | `cd <worktree> && procly env <service>` for a Port mismatch, which prints the Lease the Service has to listen on, and the command that stops the [Foreign process](daemon.md#foreign-processes) otherwise. |
| `orphans` | No removed Worktree left anything behind, see [Collecting Orphans](gc.md). | `procly gc`, which lists them. `procly gc --apply` deletes them. |
| `portless_aliases` | No [Portless alias](configuration.md#portless-aliases) Procly made outlived its Lease, which a removal that failed leaves behind. Aliases Procly did not make are not looked at. | `portless alias --remove <name>`, once for each alias. |

The Leases of a Pinned branch have no Worktree, so `leased_ports` leaves them out.

## Handing the problems to your coding agent

`procly doctor --agent <cli>` hands the problems to your own coding agent instead of leaving you to run the commands. Doctor builds a prompt that lists each failed check with its message and its commands, and runs the agent from the current directory with that prompt, the same way [`procly eject --agent`](eject.md#the-prompt) does. The agent runs in its non-interactive mode, so it applies your own permission settings. Doctor still runs no command of its own. A `warning` or a `skipped` check is not in the prompt.

| `--agent` | Command run |
| --- | --- |
| `claude` | `claude -p <prompt>` |
| `codex` | `codex exec <prompt>` |
| `gemini` | `gemini -p <prompt>` |
| `opencode` | `opencode run <prompt>` |

What the agent prints on its standard output goes to stderr, so that stdout keeps the checks, or the document with `--json`. When every check passed there is no prompt, and no agent is started. Without `--agent`, no command is ever run.

## Exit code

`procly doctor` exits with 0 when no check failed, whatever the `warning` and `skipped` ones, with 7 when at least one did, also when `--agent` handed it to your agent, since doctor does not check again, and with 125 when Procly itself fails or when the agent CLI is missing or exits with another code than 0, see [exit codes](exit-codes.md). A script can run it and branch on the code.

## JSON

`procly doctor --json` prints one document, see [JSON output](json.md#doctor). It lists the same checks in the same order, with the same fix, so that a script or an agent acts on each problem.
