# Configuration

Procly reads its configuration from 3 TOML files, merged in this order, each level overriding the one before key by key:

1. The user settings, in `$PROCLY_CONFIG_DIR/config.toml`, else `$XDG_CONFIG_HOME/procly/config.toml`, else `~/.config/procly/config.toml`. They hold personal defaults for every Repository, so they accept the `[settings]` table only.
2. `procly.toml` at the root of the Worktree, versioned with the Repository. It declares the Services the team shares.
3. `procly.local.toml` at the root of the Worktree, excluded from git. It holds personal overrides for this Worktree.

Tables merge key by key and any other value replaces the one below, so a local file can override one port without repeating the rest of the Service. A missing file counts as empty, and an unknown key is an error, so a typo never goes unnoticed. The Daemon reads the files again on every `procly up` and `procly worktree init`, no restart is needed after an edit.

## Example

```toml
[settings]
health_timeout = "2m"

[services.web]
run = "pnpm --filter web dev --port {ports.http}"
ports = { http = 3000 }
needs = ["api"]

[services.api]
run = "pnpm --filter api dev"
ports = { http = { port = 4000, pool = [4100, 4101] } }

[services.api.restart]
attempts = 3
delay = "2s"

[services.api.idle]
timeout = "1h"

[services.web.health]
path = "/api/health"
status = 200
timeout = "90s"

[services.web.env]
API_URL = "http://localhost:{services.api.ports.http}"
SESSION_COOKIE = "web_{worktree.slug}"

[services.api]
run = "pnpm --filter api dev --port {ports.http}"
ports = { http = 4000 }

[services.worker]
run = "pnpm --filter worker start"

[services.db]
compose = "postgres"
ports = { db = 5432 }
env = { DB_PORT = "{services.db.ports.db}" }

[worktree]
copy = [".env", "certs"]
setup = ["pnpm install"]

[pinned.develop]

[pinned.staging.ports]
web = { http = 3200 }
```

## `[settings]`

| Key | Default | Meaning |
| --- | --- | --- |
| `health_timeout` | `"1m"` | How long `up` waits for a Service to become healthy before stopping it and exiting with 124. |
| `restart_attempts` | `5` | How many times in a row a Service that crashed is restarted before Procly gives up. `0` never restarts it. |
| `restart_delay` | `"1s"` | The delay before the first restart. It doubles before each next one. |
| `idle_stop` | `true` | Whether a Service that printed nothing and used almost no CPU for `idle_timeout` is stopped. |
| `idle_timeout` | `"30m"` | How long a Service must be idle to be stopped. |
| `port_range` | `"24000-24999"` | The Range, bounds included, from which Worktrees other than the Main worktree draw their Leases. The default sits below the ephemeral ports of Linux and macOS. |
| `lease_expiry` | `"30d"` | How long a Lease that no Service used is kept. Past it, the Lease is dropped and its port returns to the Range. Leases of Pinned branches never expire. |
| `grace_period` | `"10m"` | How long the container of a Compose service keeps running once no Worktree needs it, see [Compose services](#compose-services). |

Durations are written as `procly run --timeout` accepts them: `500ms`, `90s`, `5m`, `1h`, `30d`, or a bare number of seconds.

## `[services.<name>]`

A Process service (`run`) or a Compose service (`compose`). The name is the one given to `procly up` and `procly down`. A Service sets exactly one of `run` and `compose`, and a key that belongs to the other kind is an error.

| Key | Required | Meaning |
| --- | --- | --- |
| `run` | one of `run` and `compose` | The command, run by `/bin/sh -c` at the root of the Worktree, in its own process group, with the environment of the `procly up` that started it. Each `{ports.<name>}` in it is replaced with the leased port of that Port name, for tools that ignore `PORT`. |
| `compose` | one of `run` and `compose` | The service of the compose file of the Repository this Service maps to, see [Compose services](#compose-services). |
| `ports` | no | The declared port of each Port name, such as `{ http = 3000, test = 3001 }`, or a table with the declared port and a Pool, such as `{ http = { port = 3000, pool = [3100, 3101] } }`. A Port name is made of lowercase letters, digits and underscores. A Service without ports, such as a queue worker, is healthy as soon as it has started. |
| `env` | no | The Derived variables of the Service, see [Derived variables](#derived-variables). |
| `needs` | no | The names of the Services to start first, such as `["db", "api"]`. See below. |
| `covers` | no | The names of the Services this Process service covers, which makes it a Group. See [Groups](#groups). |

### `needs`

`procly up web` starts what `web` needs before `web`, each one only once the Service before it is healthy, and then waits for `web` itself. A need that already runs is reused, and a need of a need is started too. Stdout holds the line of the Service you asked for alone, and stderr holds one line for each of its needs, such as `db started http://localhost:5432 port 5432 (needed by web)`.

If a need does not become healthy, `up` stops there and exits with the code of that failure (3, 4 or 124). The Service you asked for is not started. A need that is not declared, or needs that loop back to each other, make `up` fail with 125 before anything starts.

`needs` is about starting. `procly down web` leaves what `web` needs running, and a restart of `web` does not start its needs again.

## Groups

A Group is a Process service that covers other Services. It runs one command, usually the monorepo-wide dev command, as one unit, so a team can adopt Procly without splitting that command into one per app. The Services it covers stay declared with their own Port names, Derived variables and health settings, which is where the Group takes its Leases and variables from.

```toml
[services.web]
run = "pnpm --filter web dev --port {ports.http}"
ports = { http = 3000 }
env = { WEB_URL = "http://localhost:{services.web.ports.http}" }

[services.api]
run = "pnpm --filter api dev --port {ports.http}"
ports = { http = 4000 }

[services.dev]
run = "turbo dev"
covers = ["web", "api"]
```

`procly up dev` starts `turbo dev` in a process group of its own, with the environment of the caller and then, for every covered Service:

- Its Leases, as `PORT_<SERVICE>_<NAME>`, such as `PORT_WEB_HTTP` and `PORT_API_HTTP`. The plain `PORT` and `PORT_<NAME>` of a Service would collide between 2 covered Services, so a Group gets none of them. A Service name is written in upper case, every character that is not a letter or a digit becoming an underscore.
- Its Derived variables, such as `WEB_URL`.
- `PROCLY=1` and `PROCLY_SERVICE`, the name of the Group.

The Group is healthy once every covered Service answers its health check on its Lease, as it would on its own. If one never does, `up` stops the tree of the Group and exits with 124. A Port mismatch on any Lease is reported as for a Service. `procly env dev` prints the same variables without starting anything.

A Group and the Services it covers never run side by side in one Worktree:

- `procly up web` while `dev` runs starts nothing and answers `web reused (via dev) http://localhost:3000 port 3000`, with the Lease of `web`.
- `procly up dev` while `web` or `api` runs on its own is refused with exit code 125, and starts nothing. Stop them with `procly down web` first.
- `procly down web` while `dev` runs is refused with exit code 125 and points to `procly down dev`, since `web` is not a process of its own.

`up dev` starts what `dev` needs and what the Services it covers need, but not the covered Services. A need that points to a Service of the same Group is the Group itself, and a Service outside the Group that needs a covered Service starts the Group instead, since a Group never starts over a covered Service that runs alone. A Group refuses `ports`, `env`, and the `health` keys `port`, `path` and `status`, which are the ones of the Services it covers, and `health.timeout` is its own. It covers at least 1 Service, each one a declared Process service that is not itself a Group, and no Service is covered by 2 Groups. The Leases of a covered Service count as used as long as its Group runs, and are dated again when it ends.

## `procly up` without arguments

`procly up` starts every declared Service that no Group covers, each one after the Services it needs, and waits for each to be healthy before the next one starts. A Group stands for the Services it covers. It replaces the monorepo-wide dev command, with the Leases and the environment of each Service. Stdout holds one line per Service, in the order they came up, `started` or `reused`, with the URL and the port when the Service has one. A Service that does not become healthy stops the sequence and `up` exits with its code (3, 4 or 124), as for a single Service. An empty configuration makes it fail with 125.

## Compose services

A Compose service maps to a service of the compose file the Repository already has, which Procly never modifies (ADR 0004). `procly up db` runs `docker compose up --detach --wait` for that service and nothing else: Procly does not define a container, and it uses only the `docker compose` v2 CLI.

```toml
[services.db]
compose = "postgres"
ports = { db = 5432 }
env = { DB_PORT = "{services.db.ports.db}" }
```

```yaml
# compose.yaml
services:
  postgres:
    image: postgres:17
    ports:
      - "${DB_PORT:-5432}:5432"
```

| Key | Default | Meaning |
| --- | --- | --- |
| `compose` | none | The name of the service in the compose file. |
| `env` | none | The Derived variables of the Service, rendered and handed to `docker compose` for the compose file to interpolate, see [Derived variables](#derived-variables). `PORT_<NAME>` is always set too. |
| `grace_period` | `settings.grace_period` | How long the container keeps running once no Worktree needs it. |

`restart` and `idle` are for Process services and are errors on a Compose service. `needs` and `health` work as they do for a Process service, `up` waits until the container runs, or is healthy when the compose file defines a health check, and then until the health check of Procly passes.

The container is in Shared mode, the only mode of this version: one container serves every Worktree of the Repository, from the compose file of the Main worktree and under the compose project name that file gives. Its Leases are the ones of the Main worktree, so every Worktree reads the same ports, from `procly up` and from `procly env`. A Pinned branch never changes them, and they do not expire while the container runs, nor right after it stops. The Leases reach the compose file through the environment of `docker compose`: `PORT_<NAME>`, `PORT`, the Derived variables of the Service, `PROCLY` and `PROCLY_SERVICE`. The compose file must publish each Lease, which Procly checks with `docker compose config`, and `procly up` fails with 125 naming the variables the compose file may interpolate when it does not.

Containers are counted by Worktree. The first `procly up` starts the container (`started`), a `procly up` from any Worktree while it runs is a Reuse (`reused`). `procly down` lets the Worktree go of the container and prints how many other Worktrees still need it, or when it stops. Once the last Worktree let go, the container stops after the Grace period with `docker compose stop`: it is never removed, and its volumes are kept. A `procly up` before the Grace period ends keeps it running. A Worktree needs the container from its `procly up` until its own `procly down db` or `procly down`: stopping a Service that needs `db` leaves `db` counted, as `needs` is about starting.

`procly up` fails with 125 and a message that names what to install when `docker` is not on the `PATH` of the command or `docker compose` is not v2. The Daemon stops the containers it started when it exits, since it would lose count of the Worktrees that need them.

## Ports and Leases

Each Port name of a Service gets a Lease in each Worktree, a port that stays the same across restarts of the Service and of the Daemon:

- The Main worktree gets the declared port, unless another Lease or a Foreign process holds it.
- Any other Worktree, and the Main worktree when its declared port is taken, gets the first port of the Pool of the Port name that no Lease holds and that nothing listens on, checked with a real `bind`. A Pool is a short list of ports registered once with third parties, such as OAuth or WebAuthn callbacks, so that they keep working in secondary Worktrees.
- Without a Pool, or once every port of the Pool is taken, it gets the first port of `settings.port_range` that no Lease holds, that the configuration does not declare, Pools included, and that nothing listens on.

A Lease that no Service used for `settings.lease_expiry` is dropped, and its port returns to the Range. The Lease of a running Service counts as used.

One exception, recorded in ADR 0006: the Main worktree takes its declared port back as soon as it is free, even after a fallback to the Range, so a Lease of the Main worktree can change on its next start, toward its declared port only. Editing the declared port moves it the same way, and a Pinned branch does the same toward its explicit port. Every other Lease stays the same across restarts.

The Service receives its Leases in its environment, after the environment of the `procly up` that started it, so they win over the caller's:

| Variable | Value |
| --- | --- |
| `PORT` | The Lease of the Port name the health check probes. |
| `PORT_<NAME>` | The Lease of each Port name, its name in uppercase, such as `PORT_HTTP`. |

Procly also sets `PROCLY=1` and `PROCLY_SERVICE`, the name of the Service, so that a script can tell it is supervised and by which entry. Both are set last, after the Derived variables.

Once something answers on the Lease the health check probes, or once the health timeout has passed, Procly reads the TCP ports the process group of the Service listens on. When it listens on none of its Leases and on another port, `up` stops its process tree and exits with 4, a Port mismatch naming the port it listened on.

## Derived variables

`[services.<name>.env]` declares environment variables whose value is a template. They exist so that every URL embedding a port follows the Lease, and so that identifiers such as cookie names differ between Worktrees, without Procly ever touching the `.env` of the Repository (ADR 0007).

```toml
[services.web.env]
API_URL = "http://localhost:{services.api.ports.http}"
SESSION_COOKIE = "web_{worktree.slug}"
```

| Placeholder | Replaced with |
| --- | --- |
| `{services.<service>.ports.<port name>}` | The Lease of that Port name of that Service in this Worktree, granted if the Service has none yet, whether it runs or not. |
| `{worktree.slug}` | The Worktree slug, such as `app_other` for a Worktree in `app-other`. |
| `{worktree.is_main}` | `true` in the Main worktree, `false` elsewhere. |

Every other brace is left as it is, so `${HOME}` and JSON survive. A brace that opens like a placeholder of Procly (`{worktree.`, `{services.` or `{ports.`) and names nothing is an error, so that a typo never reaches a process as a literal. The own Lease of a Service is written `{services.<its name>.ports.<port name>}` in a template, the shorter `{ports.<name>}` works in `run` only.

A name must be letters, digits and underscores, not starting with a digit. `PORT`, `PORT_*`, `PROCLY` and `PROCLY_SERVICE` are Procly's own and cannot be derived. An invalid template, a Service or a Port name that is not declared, or 2 Services giving one variable 2 different templates fail `procly up` and `procly env` with exit code 125, before any process starts.

The variables are injected into the environment of the Service, so they win over the same name in the environment of the caller and over the `.env` of the Repository, which no step of Procly reads or writes. This holds for AdonisJS, Next.js and `dotenv-cli`, see ADR 0007.

## `procly env`

`procly env <service>` prints the variables Procly injects into the Service, as `export NAME='value'` lines sorted by name: the Leases (`PORT`, `PORT_<NAME>`), the Derived variables, `PROCLY` and `PROCLY_SERVICE`. It does not start the Service and prints nothing of the environment of the caller. It grants the Leases the Service needs, so the values are the ones `procly up` injects.

```console
$ eval "$(procly env web)" && pnpm --filter web test
```

## The Env file

Each `procly up` writes `.env.procly` at the root of the Worktree, for tools Procly does not launch, such as an IDE or a test runner. It holds the Derived variables of every Service of the Worktree, one `NAME=value` line each, quoted only where a dotenv parser needs it, and is readable by the user only. It leaves out `PORT`, `PORT_*`, `PROCLY` and `PROCLY_SERVICE`: those differ per Service, and `PROCLY=1` would tell a tool that Procly did not launch that it is supervised. This is why 2 Services cannot give one variable 2 templates. A value is quoted when a dotenv parser would misread it, with `\`, `"`, `$` and the line break escaped, so that the parsers that expand variables (Next.js, `dotenv-cli`) read back the value `procly env` prints. The file is rewritten only when its content changes, and it shows the configuration as of the last `procly up`: a Service that keeps running keeps the environment it started with.

## Git exclusion

The first `procly up` of a Worktree adds `.env.procly` and `procly.local.toml` to the git exclude file shared by the Worktrees of the Repository (`info/exclude` in the common git directory), as a Managed block:

```text
# >>> procly managed block: exclusions >>>
/.env.procly
/procly.local.toml
# <<< procly managed block: exclusions <<<
```

The rest of the file is kept byte for byte, and writing the block again changes nothing. Procly records the fingerprint of the content it wrote in `managed-blocks.json` of the state directory. A block whose content no longer matches was edited by you: it is left as it is and the Daemon log says so.

`procly up --gitignore` writes the block to the `.gitignore` of the Worktree instead, a file git tracks, for a team that prefers it. A later `procly up` without the flag keeps using the `.gitignore` while it holds the block, so the lines never end up in both files. The block in the exclude file is not removed by `--gitignore`, it is harmless.

## `[services.<name>.health]`

Health is a TCP connection to the Lease of the Service, on `127.0.0.1` or `::1`, optionally followed by an HTTP `GET`.

| Key | Default | Meaning |
| --- | --- | --- |
| `port` | the only Port name | The Port name to probe, required when the Service declares several. |
| `path` | none | The path of the HTTP check. Setting `path` or `status` turns the HTTP check on, with `/` as the default path. |
| `status` | `200` | The status the HTTP check must answer. |
| `timeout` | `settings.health_timeout` | The health timeout of this Service. |

## `[services.<name>.restart]`

A Service that crashes after it was healthy is restarted, so a dev server that dies on a syntax error comes back once the file is fixed. The delay before each restart doubles: 1s, 2s, 4s, 8s, 16s with the defaults. Each restart is told in the Daemon log with its delay. A Service that stays healthy for a minute starts counting from the first restart again.

A Service that dies before it was ever healthy is not restarted, `procly up` tells you at once with exit 3. When the last restart crashes too, the Service ends as failed, `procly up` exits with 3 for the one that was waiting for it, and the next `procly up` starts it afresh. `procly down`, an idle stop and a health timeout never lead to a restart.

| Key | Default | Meaning |
| --- | --- | --- |
| `attempts` | `settings.restart_attempts` | How many restarts in a row are tried before giving up. `0` never restarts this Service. |
| `delay` | `settings.restart_delay` | The delay before the first restart. |

## `[services.<name>.idle]`

A Service that printed nothing and used under 1% of a CPU (averaged since it last did) for the idle timeout is stopped, with its whole process tree. This stops the dev servers that agents forgot. The stop is not followed by a restart, and the Daemon log says why. The next `procly up` starts the Service again.

| Key | Default | Meaning |
| --- | --- | --- |
| `stop` | `settings.idle_stop` | `false` keeps this Service running however long it stays idle. |
| `timeout` | `settings.idle_timeout` | How long this Service must be idle to be stopped. |

## `[pinned.<branch>]`

A Pinned branch, such as a long-lived `develop`. The Leases of a Worktree on that branch are bound to the branch name rather than to the Worktree, so a Worktree of the branch removed and recreated, even at another path, gets the same ports. They never expire. The Main worktree is the exception: it keeps Leases of its own whatever its branch, since its declared ports are already fixed. A detached `HEAD` is on no branch, so its Worktree gets Leases of its own too. Quote a branch name holding a `/`, such as `[pinned."release/1.0"]`.

| Key | Required | Meaning |
| --- | --- | --- |
| `ports` | no | Explicit ports by Service then Port name, such as `{ web = { http = 3200 } }`, for fixed addresses. An explicit port is taken whenever no other Lease and nothing else holds it, otherwise the Port name falls back to its Pool then the Range, and gets its explicit port back once it is free. A Port name without an explicit port gets its Lease as in any Worktree. Naming a Service or a Port name that is not declared is an error. |

## `[worktree]`

What `procly worktree init` does in a fresh Worktree, see [Preparing a Worktree](worktree-init.md). Lists replace the ones of the level below instead of adding to them.

| Key | Default | Meaning |
| --- | --- | --- |
| `copy` | none | Untracked files and directories to copy from the Main worktree when the Worktree lacks them, such as env files and local certificates. Paths are relative to the root of the Worktree, an absolute path or one climbing out with `..` is an error. |
| `setup` | none | Commands run once, in order, by `/bin/sh -c` at the root of the Worktree, with the environment of the `procly worktree init` that ran them. |
