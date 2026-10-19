# Steering agents to Procly

Three things point a coding agent at Procly, and none of them happens unless you ask for it (ADR 0008):

- the Rules block, a few lines in the project's instruction file telling agents to run `procly up` instead of starting servers themselves,
- the Guard, a Claude Code hook that blocks the dev-server and compose commands Procly runs instead, and answers with the Procly command to run,
- `procly off`, which makes both stand aside for a Repository without removing anything.

```console
$ procly agent install
rules /home/me/app/AGENTS.md written
guard /home/me/app/.claude/settings.json written
$ procly agent install
rules /home/me/app/AGENTS.md unchanged
guard /home/me/app/.claude/settings.json unchanged
```

With `--json`, `agent install`, `off` and `on` print what became of each Managed block instead, with an `outcome` of `written`, `unchanged` or `edited` for a block you edited and Procly left as it is. See [JSON output](json.md).

```console
$ procly agent install --json
{"schema_version":1,"rules":{"file":"/home/me/app/AGENTS.md","outcome":"written"},"guard":{"file":"/home/me/app/.claude/settings.json","outcome":"written"},"off":false}
$ procly off --json
{"schema_version":1,"off":true,"rules":[{"file":"/home/me/app/AGENTS.md","outcome":"written"}]}
```

## `procly agent install`

It runs in the Worktree holding the current directory and writes 2 things, both inside that Worktree. Commit them like any other file, so that every Worktree of the Repository gets them.

1. The Rules block goes into `AGENTS.md`, or into `CLAUDE.md` when that is the only instruction file you have. When there is neither, it creates `AGENTS.md`. Pass `--file <path>` to name another file. The file must sit inside the Worktree, and a path that resolves outside it, through `..` or a symlink, is refused. Procly never writes into a global instruction file such as `~/.claude/CLAUDE.md`.
2. The Guard goes into `.claude/settings.json`, as a `PreToolUse` hook group on the `Bash` tool whose command is `procly guard`. The rest of the file is kept, with its keys in the same order, but the file is written again in JSON with 2 spaces of indentation, so a different layout of yours is lost.

The Rules block is a Managed block. In Markdown its markers are HTML comments, so the instruction file renders as before:

```markdown
<!-- >>> procly managed block: rules >>> -->
## Procly
(the instructions)
<!-- <<< procly managed block: rules <<< -->
```

Running the command again changes nothing when the blocks are intact. Procly records the fingerprint of what it wrote, so it can tell a block it wrote from one you edited. A block you edited is left as it is, `edited` is printed on stdout in place of `written`, and a note goes to stderr. Delete the edited block and run the command again to get the current one.

A settings file that is not JSON stops the command before anything is written, with its path on stderr.

## The Guard

Claude Code runs `procly guard` before every Bash command of the agent, with the tool call as JSON on its standard input. The Guard reads the command and does one of 2 things:

- It lets the command through by printing nothing and exiting with 0. This is what happens to every command it does not recognize, without asking the Daemon, so the hook costs nothing on `ls` or `git status`.
- For a recognized command, while the Repository is not off, it denies the call with a reason Claude Code hands to the agent:

  ```text
  `npm run dev` starts a dev server. This repository runs its dev servers through Procly, so that
  parallel worktrees never start the same one twice or fight over a port. Run `procly up <service>`
  instead. Declared Services: api, web.
  ```

The Guard never breaks the agent. It exits with 0 whatever happens, and lets the command through when it cannot judge it, when the Daemon cannot answer, when the input is not a Bash tool call, and when the directory is not in a git Repository. Only Claude Code is covered. The hooks of other agents are not verified yet.

### Recognized commands

The Guard reads each simple command of the line, so `cd apps/web && PORT=3000 npm run dev`, `nohup next dev &`, `npx vite` and `bash -c 'bun run dev'` are recognized, while a command that only appears inside quotes, as in `git commit -m 'npm run dev'`, is not.

| What it starts | Recognized commands |
| --- | --- |
| A dev server through a package script | `npm`, `pnpm`, `yarn`, `bun` and `turbo` running a script named `dev`, `start`, `serve` or `develop`, or one of them with a suffix such as `dev:web`: `npm start`, `npm run dev`, `pnpm --filter web dev`, `yarn workspace web dev`, `bun run dev`, `turbo run dev` |
| A frontend dev server | `next dev`, `next start`, `nuxt dev`, `nuxi dev`, `astro dev`, `astro preview`, `remix dev`, `vite` and `vite dev`, `vite serve`, `vite preview` (not `vite build`), `webpack serve`, `webpack-dev-server`, `ng serve`, `gatsby develop`, `nx serve`, `nx dev` |
| A backend dev server | `nodemon`, `ts-node-dev`, `tsx watch`, `node ace serve`, `adonis serve`, `rails server`, `rails s`, `flask run`, `python manage.py runserver`, `django-admin runserver`, `php artisan serve`, `uvicorn`, `gunicorn`, `hypercorn`, `hugo server`, `jekyll serve`, `ember serve` |
| Containers | `docker compose up`, `docker compose start`, `docker-compose up`, `docker-compose start`, with any option before the subcommand such as `-f`, `--profile` or `--project-name` |

A launcher in front of a command does not hide it: variables (`PORT=3000`), `env`, `nohup`, `exec`, `time`, `command`, `setsid`, the keywords `then`, `do` and `else`, `npx`, `bunx`, `pnpx`, `pnpm exec`, `pnpm dlx`, `yarn exec`, `yarn dlx`, `bun x`, `npm exec`, `bundle exec`, `poetry run` and `uv run`. Programs run by path (`./node_modules/.bin/next dev`) and `python3.12` count as their basename.

Not recognized, so let through: `npm test`, `npm run build`, `npm install`, `next build`, `vite build`, `docker compose ps`, `docker compose logs`, `docker compose run`, `docker compose down` and everything else.

The list is code, in `crates/procly/src/guard.rs`. Adding a command to it is a change to this table too.

## `procly off` and `procly on`

`procly off` is per Repository, not per Worktree: every Worktree of the Repository stands aside at once, and the state survives restarts of the Daemon.

- The Guard lets every command through.
- Every Rules block Procly wrote in a Worktree of the Repository is rewritten to say that Procly is off and that agents should start servers as usual. This shows in `git diff`, since the instruction file is tracked.
- Nothing is removed. The Guard stays in `.claude/settings.json`, and Services, Leases and containers are untouched.

`procly on` reverses it, and writes the Rules block back. Both commands are idempotent. A Rules block you edited is left as it is, and a Rules block whose file lost it is not written back. `procly agent install` run while the Repository is off writes the block that says so.

The Rules block of a Worktree is rewritten only where Procly wrote it. Another Worktree holds the committed text until you commit the new one and merge it, but its Guard stands aside right away since the state is the Repository's.

## Removing it all

`procly off` removes nothing. To take the Rules blocks, the Guard, the exclusions and the Env files out of a Repository, see [Removing Procly from a Repository](eject.md).
