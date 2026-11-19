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
$ procly on --json
{"schema_version":1,"off":false,"rules":[{"file":"/home/me/app/AGENTS.md","outcome":"written"}]}
```

## `procly agent install`

It runs in the Worktree holding the current directory and writes 2 things, both inside that Worktree. Commit them like any other file, so that every Worktree of the Repository gets them.

1. The Rules block goes into `AGENTS.md`, or into `CLAUDE.md` when that is the only instruction file you have. When there is neither, it creates `AGENTS.md`. Pass `--file <path>` to name another file. The file must sit inside the Worktree, and a path that resolves outside it, through `..` or a symlink, is refused. Procly never writes into a global instruction file such as `~/.claude/CLAUDE.md`.
2. The Guard goes into `.claude/settings.json`, as a `PreToolUse` hook group on the `Bash` tool whose command is `procly guard`. That hook group is all Procly adds: the rest of the file keeps its keys and values in the same order. Of its layout, Procly keeps the indentation of one level, tabs or spaces, and whether the file ends with a line break, then writes the whole file back one value per line at that indentation. A file already laid out that way, as most editors and Claude Code write it, gets a diff of the lines of the group plus a comma after the entry before it. An array or an object you wrote on one line is unfolded over several lines, and spacing inside a line, such as `"a":1`, becomes `"a": 1`. A file with no indented line gets two spaces and a final line break.

The Rules block is a Managed block. In Markdown its markers are HTML comments, so the instruction file renders as before:

```markdown
<!-- >>> procly managed block: rules >>> -->
## Procly
(the instructions)
<!-- <<< procly managed block: rules <<< -->
```

A block added to a file that already holds text starts after a blank line, so that its heading stands apart from your last section. In an empty file, or one Procly creates, it starts on the first line.

The block names the Services the `procly.toml` of the Worktree declares, so that an agent knows what to start without reading the configuration first. They come in the order of the file, the Groups last with the Services each one covers there. Only the versioned file counts, since your `procly.local.toml` is personal and excluded from git, and what it declares or changes must not end in an instruction file you commit:

```markdown
- The services of this repository: `api`, `web`, `worker`, and the Group `dev` covering `api` and `web`.
```

The names are read when `procly agent install` runs. After you declare, rename or remove a Service, run the command again and the block is rewritten with the new names, unless you edited it. A Repository that declares no Service gets no such line.

Running the command again changes nothing when the blocks are intact. Procly records the fingerprint of what it wrote, so it can tell a block it wrote from one you edited. A block you edited is left as it is, `edited` is printed on stdout in place of `written`, and a note goes to stderr. Delete the edited block and run the command again to get the current one.

A settings file that is not JSON stops the command before anything is written, with its path on stderr.

## The Guard

Claude Code runs `procly guard` before every Bash command of the agent, with the tool call as JSON on its standard input. The Guard reads the command and does one of 2 things:

- It lets the command through by printing nothing and exiting with 0. This is what happens to every command it does not recognize, without asking the Daemon, so the hook costs nothing on `ls` or `git status`.
- For a recognized command, while the Repository is not off, it denies the call with a reason Claude Code hands to the agent:

  ```text
  `npm run dev` starts a dev server. This repository runs its dev servers through Procly, so that
  parallel worktrees never start the same one twice or fight over a port. Run `procly up web`
  instead. Declared Services: api, web.
  ```

The Procly command in the answer is the equivalent of the blocked one, found in this order:

1. The declared Process service or Group whose `run` starts the same way as the blocked command, one being the start of the other word by word, a `{ports.<name>}` standing for any port: `npm run dev -- --host` against `run = "npm run dev"` answers `procly up web`. Both must run from the same directory. The directory of a command is the one the agent runs it in, moved by the `cd` before it, then by a directory option of its package manager, `--cwd`, `--prefix`, `--dir` or `-C`, written `--cwd apps/web` or `--cwd=apps/web`, and the directory of a Service is the one its `run` changes to the same way, the root of the Worktree when it changes to none. `bun dev` run in `apps/web`, `cd apps/web && bun dev` and `bun --cwd apps/web run dev` run from the root answer `procly up web` for a Service whose `run` is `cd apps/web && bun run dev`, while `cd apps/api && pnpm dev` only matches a `run` that changes to `apps/api` too. An absolute path, such as `cd /home/me/app/apps/web`, is placed against the root of the Worktree, the path the agent wrote first, then the canonical one, so a symlink to the Worktree leads into it. An absolute path outside the Worktree matches no Service. The directory option is not one of the words compared, and `bun dev` and `bun run dev` are the same command, and so are `pnpm dev` and `pnpm run dev`, `yarn dev` and `yarn run dev`. Another package manager is another command, so `yarn --cwd apps/web dev` does not match a `run` of `bun run dev`. The longest match wins, and 2 Services matching as long as each other give no equivalent.
2. For `docker compose up`, the Compose services whose `compose` it names, or all of them when it names none: `procly up cache && procly up db`. A Compose service maps to the compose file `compose_file` declares in the `[settings]` of `procly.toml`, or to the compose file at the root of the Worktree when none is declared, see [The compose file](configuration.md#the-compose-file). A command on another file is answered that this compose file is not managed by Procly, with no Procly command, and that answer lists no Service:
   - A file given with `-f` or `--file`, such as `docker compose -f compose.dev.yml up`, is judged from the directory the command runs in, so `-f compose.yaml` run in `apps/web` is another file than the one at the root, and `-f apps/api/docker-compose.yml` run at the root is the declared `apps/api/docker-compose.yml`. A file given with an absolute path is placed against the root of the Worktree: the managed file named that way is managed, any other absolute path, inside the Worktree or not, is not.
   - A command given no file runs the compose file Docker Compose finds from where it runs: the first of `compose.yaml`, `compose.yml`, `docker-compose.yaml` and `docker-compose.yml` in that directory, or else in the nearest directory above it, up to the root of the Worktree. So `cd apps/api && docker compose up -d` is judged against `apps/api/docker-compose.yml` when that file exists, and against the managed file when no compose file is found at all.
   - When no file is declared and the unmanaged file is inside the Worktree, the answer names the line that declares it, such as `compose_file = "apps/api/docker-compose.yml"`. Once a file is declared, a compose file at the root is no longer managed.
3. For the dev script of a whole Repository, `npm start`, `pnpm dev` or `turbo run dev` with no option narrowing it to one package, such as `--filter web` or `--filter=web`, run where the root of the Worktree is, whether the agent is there already or a `cd` brings the command back to it, `procly up`, which starts every Service no Group covers.
4. Otherwise `procly up <service>`, the agent picking among the declared Services the answer lists.

The Guard never breaks the agent. It exits with 0 whatever happens, and lets the command through when it cannot judge it, when the Daemon cannot answer, when the input is not a Bash tool call, and when the directory is not in a git Repository. Only Claude Code is covered. The hooks of other agents are not verified yet.

### Recognized commands

The Guard reads each simple command of the line, so `cd apps/web && PORT=3000 npm run dev`, `nohup next dev &`, `npx vite` and `bash -c 'bun run dev'` are recognized, while a command that only appears inside quotes, as in `git commit -m 'npm run dev'`, is not.

| What it starts | Recognized commands |
| --- | --- |
| A dev server through a package script | `npm`, `pnpm`, `yarn`, `bun` and `turbo` running a script named `dev`, `start`, `serve` or `develop`, or one of them with a suffix such as `dev:web`: `npm start`, `npm run dev`, `pnpm --filter web dev`, `yarn workspace web dev`, `bun run dev`, `bun --cwd apps/web run dev`, `turbo run dev` |
| A frontend dev server | `next dev`, `next start`, `nuxt dev`, `nuxi dev`, `astro dev`, `astro preview`, `remix dev`, `vite` and `vite dev`, `vite serve`, `vite preview` (not `vite build`), `webpack serve`, `webpack-dev-server`, `ng serve`, `gatsby develop`, `nx serve`, `nx dev` |
| A backend dev server | `nodemon`, `ts-node-dev`, `tsx watch`, `node ace serve`, `adonis serve`, `rails server`, `rails s`, `flask run`, `python manage.py runserver`, `django-admin runserver`, `php artisan serve`, `uvicorn`, `gunicorn`, `hypercorn`, `hugo server`, `jekyll serve`, `ember serve` |
| Containers | `docker compose up`, `docker compose start`, `docker-compose up`, `docker-compose start`, with any option before the subcommand such as `-f`, `--profile` or `--project-name` |

A launcher in front of a command does not hide it: variables (`PORT=3000`), `env`, `nohup`, `exec`, `time`, `command`, `setsid`, the keywords `then`, `do` and `else`, `npx`, `bunx`, `pnpx`, `pnpm exec`, `pnpm dlx`, `yarn exec`, `yarn dlx`, `bun x`, `npm exec`, `bundle exec`, `poetry run` and `uv run`, and the wrappers that load variables for a command, `cross-env`, `cross-env-shell`, `dotenv`, `dotenvx run` and `env-cmd`. `cross-env` and `cross-env-shell` are read past their `NAME=value` words and their options, and `dotenv`, `dotenvx run` and `env-cmd` past their options with their values and one `--` right after them, so `cross-env PORT=3000 bun run dev` and `dotenv -e .env -- next dev` are recognized. A `--` further on belongs to the command, as in `dotenv -e .env npm run dev -- --port 3001`. The command line `cross-env-shell` hands to a shell, as in `cross-env-shell NODE_ENV=dev "next dev"`, is read like the line the Guard receives. Programs run by path (`./node_modules/.bin/next dev`) and `python3.12` count as their basename.

Not recognized, so let through: `npm test`, `npm run build`, `npm install`, `next build`, `vite build`, `docker compose ps`, `docker compose logs`, `docker compose run`, `docker compose down` and everything else.

The list is code, in `crates/procly/src/guard.rs`. Adding a command to it is a change to this table too.

## `procly off` and `procly on`

`procly off` is per Repository, not per Worktree: every Worktree of the Repository stands aside at once, and the state survives restarts of the Daemon.

- The Guard lets every command through.
- Every Rules block Procly wrote in a Worktree of the Repository is rewritten to say that Procly is off and that agents should start servers as usual. This shows in `git diff`, since the instruction file is tracked.
- Nothing is removed. The Guard stays in `.claude/settings.json`, and Services, Leases and containers are untouched.

`procly on` reverses it, and writes the Rules block back, with the Services the `procly.toml` of its Worktree declares at that moment, as `procly agent install` would. The block that says Procly is off names none. Both commands are idempotent. A Rules block you edited is left as it is, and a Rules block whose file lost it is not written back. `procly agent install` run while the Repository is off writes the block that says so.

The Rules block of a Worktree is rewritten only where Procly wrote it. Another Worktree holds the committed text until you commit the new one and merge it, but its Guard stands aside right away since the state is the Repository's.

## Removing it all

`procly off` removes nothing. To take the Rules blocks, the Guard, the exclusions and the Env files out of a Worktree or a Repository, see [Removing Procly from a Repository](eject.md).
