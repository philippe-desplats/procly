# Logical isolation first, chosen per repository, through declarative drivers

A Compose service defaults to Logical isolation when its technology allows it (a database per Worktree in Postgres, a database index in Redis, a bucket in an S3-compatible store), and stays Shared otherwise. Isolated is used only where it is declared, since it runs one container per Worktree and cannot work with a compose file that fixes `container_name`. Mailpit runs Shared. One container per technology keeps RAM in check across many worktrees, while per-Worktree resources keep branches with different migrations apart.

The Isolation mode is set per Service and per Repository, because Procly cannot prove that an app reads its Logical resource from the environment: one known app hardcodes Redis `db: 0`, so it needs Isolated.

Drivers are declarative TOML files (create, check and drop commands run through `docker compose exec`, a URL template, a capacity such as 16 Redis indexes). The bundled drivers use the same format, so adding a technology never needs a new Procly release. Executable drivers speaking JSON over stdin and stdout are added only on real need.
