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
