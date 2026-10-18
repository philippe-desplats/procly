# Reversible by design: Off, Eject and fingerprinted Managed blocks

A Repository must keep working exactly as before Procly, with its original scripts, at any time. Procly never edits a tracked file without an explicit command, and writes only Managed blocks delimited by markers and recorded with a fingerprint of their content. `procly off` makes Guards and Rules blocks stand aside. `procly eject` removes intact Managed blocks mechanically and, for blocks the user edited, generates a prompt describing what to undo, which `--agent <cli>` hands to the user's own coding agent.

## Consequences

- Procly never writes to the user's global agent instruction files on its own, unlike Portly's onboarding.
- Existing setup scripts become one-line wrappers around `procly worktree init`, so the previous script stays recoverable from git.
