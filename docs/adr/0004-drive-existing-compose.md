# Drive the repository's existing compose file through the docker compose v2 CLI

Procly never defines containers itself and never rewrites a compose file. It runs `docker compose` with a per-Worktree project name when Isolated, and passes Leases through environment variables that the compose file reads (`${DB_PORT:-5432}`). Only the `docker compose` v2 CLI is supported, which covers Docker Desktop, OrbStack, Colima and Podman in compatibility mode, with no Docker SDK and no separate `podman-compose`.

## Consequences

- A compose file that fixes `container_name` or a network name cannot run Isolated. Procly detects it through `docker compose config`, refuses that mode with the offending line, and still accepts Shared and Logical.
- Procly never deletes data by itself. Volumes follow the Retention policy, which defaults to keep.
