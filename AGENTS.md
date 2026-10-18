# Procly

Procly is a local supervisor that coding agents call instead of starting dev processes and containers themselves, so that parallel agents and parallel git worktrees never duplicate a process, a container or a port.

See `GLOSSARY.md` for the domain glossary (ubiquitous language), and use its terms, not their synonyms, in code, tickets and documentation. Structuring decisions are recorded in `docs/adr/`.

## Project conventions

### Issue tracker

Issues and specs live in GitHub Issues of `philippe-desplats/procly`, managed with the `gh` CLI. See `docs/conventions/issue-tracker.md`.

### Triage labels

The 5 default triage labels: `needs-triage`, `needs-info`, `ready-to-build`, `needs-judgment`, `wontfix`. See `docs/conventions/triage-labels.md`.

### Domain docs

Single-context: `GLOSSARY.md` and `docs/adr/` at the repository root. See `docs/conventions/domain.md`.

## Rust and Tauri

Procly is a Rust workspace (ADR 0002): crates live under `crates/`, the toolchain is pinned in `rust-toolchain.toml`.

Read the project rules before writing code in their area:

- `docs/rules/rust.md`: crate layout, errors, async runtime, unsafe, tests, checks.
- `docs/rules/processes.md`: spawning, stopping, output capture and PTYs for supervised processes.
- `docs/rules/tauri.md`: Tauri v2 conventions for the desktop app.

The official docs of `process-wrap`, `portable-pty` and Tauri v2 are vendored and indexed in `docs/reference/README.md`, at the versions the project depends on. Read the API there rather than from memory. Regenerate them with `make docs`.

Run `make check` (format, Clippy and rustdoc with warnings denied, tests) before pushing. The CI runs the same targets on macOS and Linux.
