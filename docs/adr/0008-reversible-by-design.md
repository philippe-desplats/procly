# Reversible by design: Off, Eject and fingerprinted Managed blocks

A Repository must keep working exactly as before Procly, with its original scripts, at any time. Procly never edits a tracked file without an explicit command, and writes only Managed blocks delimited by markers and recorded with a fingerprint of their content. `procly off` makes Guards and Rules blocks stand aside. `procly eject` removes intact Managed blocks mechanically and, for blocks the user edited, generates a prompt describing what to undo, which `--agent <cli>` hands to the user's own coding agent.

## Consequences

- Procly never writes to the user's global agent instruction files on its own, unlike Portly's onboarding.
- Existing setup scripts become one-line wrappers around `procly worktree init`, so the previous script stays recoverable from git.
- The Guard is a Managed block that holds no markers, since JSON allows no comments. It is identified by its content, the hook group whose command is `procly guard`, and by the fingerprint of that group recorded when Procly wrote it. A group with the recorded fingerprint is Procly's and is removed mechanically, a group the user edited is left in place and described in the generated prompt, as for any other Managed block. The fingerprint is FNV-1a over 64 bits of the content, stored with a version prefix so that a later Procly compares it the same way.
