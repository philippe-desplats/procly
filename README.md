<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/brand/procly-horizontal-couleur-sombre.svg">
  <img src="docs/brand/procly-horizontal-encre.svg" alt="Procly" height="64">
</picture>

**A local supervisor for coding agents, built for parallel git worktrees**

Agents call `procly up` instead of starting dev servers and containers themselves, so parallel agents and parallel worktrees never duplicate a process, a container or a port.

[![CI](https://github.com/philippe-desplats/procly/actions/workflows/ci.yml/badge.svg)](https://github.com/philippe-desplats/procly/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/philippe-desplats/procly?sort=semver)](https://github.com/philippe-desplats/procly/releases)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/License-MIT%20OR%20Apache--2.0-blue.svg)](#license)

<img src="docs/demo.gif" alt="Procly in action" width="720" />

</div>

2 agents in 2 worktrees of the same repository both run `npm run dev`. One gets `EADDRINUSE`, the other quietly takes another port that nothing else knows about, and the server the first agent started an hour ago is still running. Procly puts one authority in front of all of it.

## Why

Coding agents are good at starting servers and bad at keeping track of them. Each agent sees its own shell, not the dev server another agent left running, nor the worktree next door that declared the same port. Run several agents in parallel worktrees and you get duplicate servers, port collisions, containers started twice and processes nobody stops.

Procly is a single per-user Daemon that owns every Service, port and container across all your Repositories and Worktrees. A Service is declared once in `procly.toml`, and `procly up web` either starts it or answers that it already runs, with where to reach it. Each Worktree gets its own persistent ports, called Leases, so a second Worktree never fights the first one for port 3000. The agent gets one command, a stable exit code and a line it can read, and the Daemon does the bookkeeping.

## Features

- **One Daemon for everything**: a Service runs at most once per Worktree, and a second `procly up` is a Reuse of the running one, never a duplicate.
- **Persistent Leases per Worktree**: the Main worktree keeps the ports `procly.toml` declares, other Worktrees get a Pool port or one from the Range (`24000-24999` by default), and the port stays the same across restarts.
- **Derived variables**: URLs and names that embed a port or the Worktree follow the Lease, injected into the Service and mirrored in a git-excluded `.env.procly`. Your `.env` is never rewritten.
- **Health checks and Port mismatch**: `up` returns once a TCP or HTTP check passes on the Lease, and stops a Service that listens on another port than the one it was given.
- **Restart and idle stop**: a Service that crashes after it was healthy is restarted with backoff, and one that stays silent and idle for 30 minutes is stopped, so forgotten dev servers do not pile up.
- **Jobs**: `procly run` starts a bounded command deduplicated by its Fingerprint, and `procly wait` returns its real exit code.
- **Compose services**: Procly drives the compose file you already have through `docker compose` v2, never modifies it, and shares one container between Worktrees, counted by Worktree and stopped after a Grace period.
- **Logical resources**: a Driver gives each Worktree its own Postgres database or Redis index inside that one container. Postgres and Redis Drivers ship with Procly, and a new technology is a TOML file.
- **Groups**: keep your monorepo-wide dev command as one unit that covers several Services.
- **Agent steering, opt-in**: `procly agent install` writes a Rules block into `AGENTS.md` and a Guard into `.claude/settings.json`, a Claude Code hook that blocks a dev-server command and answers with the Procly command to run.
- **Reversible**: `procly off` makes the Guard and the Rules block stand aside, and `procly eject` removes everything Procly wrote into a Repository.
- **Crash tolerant**: Services run in process groups of their own and outlive a crashed Daemon, which the next Daemon re-attaches. A launchd agent or systemd user unit can start the Daemon with your session.
- **Built for agents to parse**: stable exit codes, and a versioned `--json` document on every command but the Guard.

## Installation

Procly is one binary, `procly`, for macOS (Apple silicon and Intel) and Linux (x86_64 and arm64). It needs no runtime, and the Daemon starts on demand, from the first command that needs it. Windows is not supported.

### Homebrew

```sh
brew install philippe-desplats/procly/procly
procly --version
```

The formula lives in the tap [`philippe-desplats/homebrew-procly`](https://github.com/philippe-desplats/homebrew-procly) and installs the binary of the matching GitHub release.

### GitHub Releases

Each [release](https://github.com/philippe-desplats/procly/releases) carries one archive per platform and a `SHA256SUMS` file:

| Platform | Archive |
| --- | --- |
| macOS, Apple silicon | `procly-<version>-aarch64-apple-darwin.tar.gz` |
| macOS, Intel | `procly-<version>-x86_64-apple-darwin.tar.gz` |
| Linux, arm64 | `procly-<version>-aarch64-unknown-linux-gnu.tar.gz` |
| Linux, x86_64 | `procly-<version>-x86_64-unknown-linux-gnu.tar.gz` |

```sh
version=0.2.0
target=aarch64-apple-darwin
base=https://github.com/philippe-desplats/procly/releases/download/v$version
curl -fsSLO $base/procly-$version-$target.tar.gz
curl -fsSLO $base/SHA256SUMS
grep " procly-$version-$target.tar.gz\$" SHA256SUMS | shasum -a 256 --check
tar -xzf procly-$version-$target.tar.gz
install procly-$version-$target/procly /usr/local/bin/procly
```

On Linux, `sha256sum --check` replaces `shasum -a 256 --check`. The Linux archives are built on Ubuntu 22.04 and need glibc 2.35 or newer.

### From source

`cargo install` needs Rust 1.99 or newer, the `rust-version` of the crate, and refuses an older compiler.

```sh
cargo install --locked --git https://github.com/philippe-desplats/procly --tag v0.2.0 --bin procly
```

Leave out `--tag` to install the head of `main`. From a clone, `rustup` uses the toolchain pinned in [`rust-toolchain.toml`](rust-toolchain.toml), and `cargo install --locked --path crates/procly --bin procly` does the same. See [Installing Procly](docs/install.md) for the details.

### Optional tools

- `docker compose` v2, only for Compose services.
- `lsof` and a running Docker engine, for `procly status` to list Foreign processes and containers.

## Usage

Declare your Services in `procly.toml` at the root of the Repository (or let `procly init` propose one), then:

```console
$ procly up web
api started http://localhost:24081 port 24081 (needed by web)
web started http://localhost:24080 port 24080
$ procly up web
api reused http://localhost:24081 port 24081 (needed by web)
web reused http://localhost:24080 port 24080
```

Run from another Worktree of the same Repository, the same command starts its own copy on its own Leases. The daily commands:

```sh
procly up web              # start web and what it needs, wait until healthy, print where to reach each of its ports
procly up                  # start every declared Service, each after what it needs
procly env web             # print the variables Procly injects into web, without starting it
procly logs web            # the recent output of web
procly status              # the Services and Jobs of this Worktree
procly status --all        # every Worktree of every Repository the Daemon knows
procly down web            # stop web with its whole process tree
procly down                # stop every Service of this Worktree

job=$(procly run "npm test")   # start a Job, or find the same one running
procly wait "$job"             # print its output and exit with its exit code
```

Setting up a Repository and its Worktrees:

```sh
procly init                # propose a procly.toml from the dev scripts and the compose file
procly worktree init       # prepare a fresh Worktree: copy untracked files, run setup commands
procly agent install       # write the Rules block and the Guard, nothing is written without it
procly off                 # make the Guard and the Rules block stand aside (procly on reverts)
procly eject               # remove everything Procly wrote into this Repository
procly unit install        # start the Daemon with your session (launchd or systemd)
```

Every command but `guard` accepts `--json` and then prints one document carrying a `schema_version`. Exit codes are stable, so an agent can branch on them without parsing the output: 0 on success, 3 when a Service ended before it was healthy, 4 on a Port mismatch, 5 on a conflict with something running outside Procly, 124 on a timeout, 125 when Procly itself fails. See [Exit codes](docs/exit-codes.md) and [JSON output](docs/json.md).

To point your agents at Procly, run `procly agent install` once and commit what it writes. The Guard covers Claude Code: it reads each Bash call through a `PreToolUse` hook, lets unknown commands through without asking the Daemon, and denies a known dev-server or `docker compose up` command with the equivalent Procly command. See [Steering agents to Procly](docs/agents.md).

## Try it without touching your setup

The repository ships a playground. It builds a throwaway `HOME` under `/tmp`, holding a copy of [`sample/project`](sample/project) as a git repository with a second Worktree, and runs a separate Daemon whose state directory is inside it, so your real Procly state is never touched. The sample Services need nothing but `python3`.

```sh
source sample/demo.sh            # builds procly with cargo, or uses the one on your PATH
procly up web                    # in ~/acme-shop, the Main worktree
cd ../acme-shop-checkout
procly up web                    # the same Services on other ports
procly status --all
procly_demo_cleanup              # stop the demo Daemon and remove the temp dir
```

See [`sample/README.md`](sample/README.md) for details and for regenerating the demo GIF with [VHS](https://github.com/charmbracelet/vhs).

## Configuration

`procly.toml` is versioned with the Repository and declares the Services the team shares. `procly.local.toml`, excluded from git, holds personal overrides for one Worktree, and `config.toml` in `$PROCLY_CONFIG_DIR`, else `$XDG_CONFIG_HOME/procly`, else `~/.config/procly`, holds personal `[settings]` for every Repository. The Daemon reads them again on every `procly up`, no restart needed.

```toml
[services.web]
run = "pnpm --filter web dev --port {ports.http}"   # {ports.http} is the Lease of this Worktree
ports = { http = 5173 }
needs = ["api"]

[services.web.env]
API_URL = "http://localhost:{services.api.ports.http}"

[services.api]
run = "pnpm --filter api dev --port {ports.http}"
ports = { http = { port = 4000, pool = [4100, 4101] } }   # a Pool for registered callback ports
needs = ["db"]

[services.api.env]
DATABASE_URL = "{services.db.resources.dev.url}"

[services.api.health]
path = "/health"

[services.db]
compose = "postgres"        # a service of the compose file you already have
driver = "postgres"         # one database per Worktree in the one container
ports = { db = 5432 }

[services.db.resources.dev]
name = "app"

[worktree]
copy = [".env"]             # copied from the Main worktree by `procly worktree init`
setup = ["pnpm install"]
```

An unknown key is an error, so a typo never goes unnoticed. [Configuration](docs/configuration.md) documents every key, Groups, Compose services, Drivers, Pinned branches and the `[settings]` table, such as `port_range`, `health_timeout` and `idle_timeout`.

## How it works

- The first `procly` command that needs the Daemon starts it, one per user (`procly init` and the Guard on a command it does not recognize never do), which listens on a Unix socket in its state directory (`$PROCLY_STATE_DIR`, else `$XDG_STATE_HOME/procly`, else `~/.local/state/procly`) and never on a TCP port. The CLI is a client of that socket.
- `procly up` resolves the Repository and the Worktree from the current directory, grants the Leases, starts the Service by `/bin/sh -c` in a process group of its own with its Leases and Derived variables in the environment, and waits for its health check.
- Leases, running Services and containers are recorded as atomic JSON documents in the state directory. A Daemon that crashed leaves its Services running, and the next one re-attaches those still listening on their Lease and stops what is left of the others.
- Stopping a Service sends `SIGTERM` to its whole process group, then `SIGKILL`, so nothing it forked survives it.
- Compose services go through the `docker compose` v2 CLI and the compose file of the Repository, which Procly never modifies.

The structuring decisions are recorded in [`docs/adr/`](docs/adr), the vocabulary in [`GLOSSARY.md`](GLOSSARY.md). See also [The Daemon across crashes and sessions](docs/daemon.md), [Preparing a Worktree](docs/worktree-init.md), [Proposing a configuration](docs/init.md) and [Removing Procly from a Repository](docs/eject.md).

## Development

```sh
make build        # cargo build --workspace --all-targets --locked
make check        # format check, Clippy and rustdoc with warnings denied, then the tests
make fmt          # apply the formatting
```

The tests drive the built binary, each against a Daemon of its own in a temporary state directory, so they never touch a Daemon you run. Tests that need Docker skip themselves when `docker compose` v2 is unavailable.

## Contributing

Bug reports, ideas and pull requests are welcome. See [CONTRIBUTING.md](CONTRIBUTING.md) for how to report issues, the contribution flow, and the requirements a change must meet to be merged.

## Security

See [SECURITY.md](SECURITY.md) for how to report a vulnerability, the threat model and the project's supply-chain posture.

## Acknowledgements

Procly reuses ideas from Portly, a local process supervisor for coding agents released under the MIT license: its proven mechanics for PTYs, process-group stop, restart backoff, health checks, waiting on a Job with its real exit code, and port-occupant and Docker-publisher lookup. Procly is not a fork of Portly and rewrites those mechanics in Rust, see [ADR 0001](docs/adr/0001-own-supervisor-single-authority.md) and [`NOTICE`](NOTICE). Thanks also to the authors of [`process-wrap`](https://github.com/watchexec/process-wrap), [`portable-pty`](https://github.com/wezterm/wezterm/tree/main/pty) and [Tokio](https://tokio.rs), which Procly is built on.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <https://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <https://opensource.org/licenses/MIT>)

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
