# Contributing to Procly

Thanks for taking the time to contribute. This document explains how to report issues, propose changes, and the requirements your contribution must meet to be merged.

## Reporting bugs and requesting features

Open a [GitHub issue](https://github.com/philippe-desplats/procly/issues). For a bug, include your OS and architecture, the output of `procly --version`, the `procly.toml` involved (or the smallest one that reproduces it), the exact command you ran with its exit code, and what you expected versus what happened. The end of `daemon.log`, in the state directory (`~/.local/state/procly` by default), often holds the reason. For security issues, do not open a public issue: follow [SECURITY.md](SECURITY.md) instead.

## Proposing a change

1. Fork the repository and create a branch from `main`.
2. Make your change, with tests for any new behavior or bug fix.
3. Make sure `make check` passes locally (see below).
4. Open a pull request describing what changed and why. Link the issue it addresses if there is one.

Small fixes (typos, docs, obvious bugs) can go straight to a pull request. For larger changes, open an issue first to discuss the direction before investing time. A change that reverses a decision recorded in [`docs/adr/`](docs/adr) needs a new ADR.

## Requirements for acceptable contributions

A contribution is merged only once it meets all of the following:

- **Checks**: `make check` passes. It runs `cargo fmt --all --check`, Clippy on every target with warnings denied (the `pedantic` group is on for the whole workspace), rustdoc with warnings denied, and the tests. The CI runs the same `make` targets on macOS and Linux.
- **Lint exceptions**: silence a lint with `#[expect(<lint>, reason = "...")]` on the smallest item, never with a bare `#[allow]`, so the reason is recorded and the build fails once the exception is no longer needed. `unsafe_code` is denied, see [`docs/rules/rust.md`](docs/rules/rust.md).
- **Tests**: new behavior and bug fixes ship with tests. The CLI is the contract, so integration tests in `crates/procly/tests/` drive the built binary, each against a Daemon of its own with an isolated state directory, and never touch the state directory of a Daemon you run. Pure logic gets unit tests in its module. A test that needs Docker is named `docker_*` and skips itself when `docker compose` v2 is unavailable.
- **Vocabulary**: code, docs and messages use the terms of [`GLOSSARY.md`](GLOSSARY.md), such as Service, Job, Lease and Worktree, not their synonyms.
- **Contract**: exit codes and `--json` documents are a public interface. A change to [`docs/exit-codes.md`](docs/exit-codes.md) is a breaking change of the CLI, and an incompatible change to a JSON document bumps its `schema_version`, see [`docs/json.md`](docs/json.md). Update the docs under `docs/` in the same pull request as the behavior they describe.
- **Commits**: messages follow [Conventional Commits](https://www.conventionalcommits.org/), for example `feat: procly init proposes a procly.toml` or `fix: never lose Service output while the Daemon empties its output file`. The history uses `feat`, `fix`, `docs`, `test` and `refactor`, with `ci` and `chore` for workflows and tooling.
- **Cross-platform**: Procly targets macOS and Linux, on arm64 and x86_64. Keep platform-specific code behind `cfg` attributes or runtime checks, and never assume a tool of one platform is present on the other.
- **No AI or vendor attribution** in commits, code comments, or documentation.

## Local development

The toolchain is pinned in [`rust-toolchain.toml`](rust-toolchain.toml), and `rustup` installs it with `rustfmt` and Clippy on the first build.

```sh
make build        # cargo build --workspace --all-targets --locked
make fmt          # apply the formatting
make check        # what the CI runs, run it before pushing
```

The project rules live in [`docs/rules/`](docs/rules): [`rust.md`](docs/rules/rust.md) for crate layout, errors, async and tests, [`processes.md`](docs/rules/processes.md) for spawning, stopping and output capture. The API of `process-wrap` and `portable-pty` at the pinned versions is vendored under [`docs/reference/`](docs/reference/README.md), regenerated with `make docs`.

To try your build without touching your real Procly state, use the self-contained playground, which builds this checkout:

```sh
source sample/demo.sh
procly up web
procly_demo_cleanup
```

## License

Procly is licensed under either of the [MIT license](LICENSE-MIT) or the [Apache License, Version 2.0](LICENSE-APACHE), at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
