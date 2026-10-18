# No per-session ownership of services in v0.1

Any agent in a Worktree may stop a Service another agent started, and the Daemon logs who asked. Counting holders per agent session would protect a running test from another agent's stop, but no reliable session identity exists across Claude Code, Codex, Pi, Paperclip and Hermes. The idle stop frees what nobody uses. Holders are reconsidered if real usage shows harmful interruptions.
