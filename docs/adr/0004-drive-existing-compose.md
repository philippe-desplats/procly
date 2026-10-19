# Drive the repository's existing compose file through the docker compose v2 CLI

Procly never defines containers itself and never rewrites a compose file. It runs `docker compose` with a per-Worktree project name when Isolated, and passes Leases through environment variables that the compose file reads (`${DB_PORT:-5432}`). Only the `docker compose` v2 CLI is supported, which covers Docker Desktop, OrbStack, Colima and Podman in compatibility mode, with no Docker SDK and no separate `podman-compose`.

## Consequences

- A compose file that fixes `container_name` or a network name cannot run Isolated. Procly detects it through `docker compose config`, refuses that mode with the offending line, and still accepts Shared and Logical.
- Procly never deletes data by itself. Volumes follow the Retention policy, which defaults to keep.
- A Logical resource whose command Seed fails is dropped again when the same request created it, so that the next `procly up` seeds it afresh instead of finding it and keeping it empty. The request fails with a message that says whether the drop worked, and gives the command to drop it by hand when it did not. A resource the container already had is never dropped, and the resources created earlier in the same request stay, as the request stops at the failed one.
