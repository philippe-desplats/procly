# Procly discovers worktrees and never creates them

Procly finds Worktrees through `git` and integrates with the tools that manage them, instead of competing with them. Worktrunk, Orca and plain `git worktree add` all prepare a Worktree by calling `procly worktree init`, and teardown hooks release Leases and stop Services. Portless can later alias a Lease to a stable name. This keeps Procly usable from any orchestrator, and keeps the worktree lifecycle out of its scope.
