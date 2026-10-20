# Proposing a configuration

`procly init` reads the `dev` scripts of the `package.json` files and the compose file of a Worktree, and proposes a `procly.toml` for it. It is the way into Procly for a Repository that has none, see [Configuration](configuration.md) for what the file can declare.

```console
$ cd ~/code/acme
$ procly init
# Proposed by `procly init`. Review it before you commit it, `procly up` starts what it declares.

[services.dev]
run = "bun run dev"
covers = ["web"]

[services.web]
run = 'cd apps/web && export PATH="$PWD/node_modules/.bin:$PWD/../../node_modules/.bin:$PATH" && next dev --port {ports.http}'

[services.web.ports]
http = 3001
procly: wrote /home/me/code/acme/procly.toml
```

The proposal is printed on stdout and written to `procly.toml` at the root of the Worktree you run it in, whatever the directory you run it from. The status goes to stderr. Init reads the Worktree only, so it never starts the Daemon.

With `--json`, stdout holds 1 document with the `file`, the `toml` proposed and whether it was `written`, which is `false` when a `procly.toml` existed. See [JSON output](json.md).

```console
$ procly init --json
{"schema_version":1,"file":"/home/me/code/acme/procly.toml","toml":"# Proposed by `procly init`. Review it before you commit it, `procly up` starts what it declares.\n\n[services.app]\nrun = \"npm run dev\"\n","written":true}
```

A `procly.toml` that exists is never overwritten. The proposal is still printed, stderr says that the file was left as it is, and the command exits with 0. The file is created exclusively, so even one that appears while the proposal is made is kept. A Worktree with no `dev` script and no compose file gets nothing written and exits with 125, see [exit codes](exit-codes.md).

The proposal declares what the Worktree states and leaves out what it would have to invent. Read it, then run `procly up`. Procly reads nothing else, so a Repository that starts its servers through a `Procfile` or a `Makefile` gets no proposal for them.

## Package scripts

A package with a `dev` script becomes a Process service named after the package, without its scope (`@acme/web` is `web`). It runs from the root of the Worktree, through the package manager that its lockfile tells (`pnpm-lock.yaml`, `bun.lock` or `bun.lockb`, `yarn.lock`, else npm): `cd apps/web && pnpm run dev`.

A Worktree is a monorepo when it declares workspaces. Procly reads the `workspaces` of the root `package.json`, as a list or as a table with `packages`, and the `packages` of `pnpm-workspace.yaml`. A pattern is a path whose segments are names, a lone `*` for any directory or a lone `**` for any depth of them, such as `apps/*` or `packages/**`, and a `!` pattern leaves directories out. Hidden directories and `node_modules` are not looked into. A workspace package with a `dev` script becomes a Service, and the `dev` script of the root package becomes a Group named `dev` that covers all of them, so that the command the team already runs stays one command. A monorepo whose root has no `dev` script gets its Services and no Group.

A name that is taken gets a number, so a package called `dev` leaves `dev` to itself and the Group becomes `dev-2`.

### Ports named in a script

A `dev` script that names one port, with `--port 3001`, `--port=3001`, `-p 3001` or `PORT=3001`, gives its Service a Port name `http` declared at that port, which the Main worktree keeps. The package manager would run the script with the port written in it, so the proposal runs the script itself instead, with the number replaced by `{ports.http}` and the binaries of the package and of the root put on the `PATH`, as the package manager does. The script is not run through `npm run`, so its `predev` and `postdev` hooks do not run.

A port below 1024, a script that names several different ports, and any other way of writing a port are not taken, the Service then runs through the package manager and declares no port. Add `ports = { http = <port> }` and the placeholder where the script reads its port, so that parallel Worktrees do not share it.

A Group runs the `dev` script of the root as it is. The Services it covers get their Leases in `PORT_<SERVICE>_<NAME>`, but a script that writes its port still binds that port in a Worktree other than the Main one, which `up` then reports as a Port mismatch. Point the root command at the Leases, or start the Services one by one.

## The compose file

Procly looks for `compose.yaml`, `compose.yml`, `docker-compose.yaml` and `docker-compose.yml`, in that order, and reads the first as written, without Docker and without resolving its variables. Every service of the file becomes a Compose service of the same name, and nothing in the file is modified (ADR 0004).

- A port published on a host port is declared under a Port name. The well-known port of a known technology has its usual name (`db` for Postgres, `redis`, `smtp` and `http` for Mailpit). A port that the compose file reads from `${PORT_<NAME>:-5432}` is named `<name>`. Any other is `port`, or `port_<container port>` among several.
- A port written `${DB_PORT:-5432}` is declared at its default, and `DB_PORT` is derived from the Lease of its Port name, so that the compose file keeps interpolating the variable it already has. A variable that 2 services read is derived for the first only, since the Env file holds one template for it.
- A service that publishes several ports gets the `health` port that its technology probes, else the first.
- Ports with no host port, ranges, UDP ports and variables without a default are left out.

Known technologies run in the mode that keeps their data apart at the least cost, see [Logical resources and Drivers](configuration.md#logical-resources-and-drivers). A Postgres image (`postgres`, `postgis`, `pgvector`, `timescaledb` and `postgresql`) that publishes a port is proposed Logical when its `POSTGRES_USER` and `POSTGRES_PASSWORD` are unset or `postgres`, the credentials the bundled Driver builds its URLs with, otherwise it is Shared, with the Driver `postgres` and a `dev` resource named after its `POSTGRES_DB`, else `postgres`. The Services that use the database still have to read it, for example `DATABASE_URL = "{services.postgres.resources.dev.url}"` in their `env`. Every other service is proposed Shared. A Redis image (`redis`, `redis-stack`) that publishes a port is Logical too, with the Driver `redis` and a resource `main`, which the Driver numbers, so no `name` is set. Valkey and KeyDB, which the bundled Driver is not written for, are Shared.

An image is recognised by its name alone, wherever it comes from and whatever its tag.
