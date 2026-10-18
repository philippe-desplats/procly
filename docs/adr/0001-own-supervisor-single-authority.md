# Build our own supervisor as a single global authority

Procly is a new supervisor, not a fork of Portly and not a layer over process-compose. Preventing duplicates across parallel agents and git worktrees needs one Daemon per user that sees every Repository and Worktree at once. Portly has no worktree, lease or compose model, and fixing that would rewrite its data model in 2 diverging implementations (Swift and Go). process-compose runs one instance per project, so state would be split across N instances that Procly would still have to arbitrate.

## Considered Options

- Fork Portly: rejected, see above. Its proven mechanics (PTY, process-group stop, restart backoff, health checks, job wait with the real exit code, port-occupant and Docker-publisher lookup) are kept as reference, with credit.
- Build on process-compose: rejected for the split-state reason.
- Contribute to workz or Docktree: rejected, neither supervises host processes.
