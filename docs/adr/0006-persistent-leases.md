# Persistent port leases keyed by worktree, or by branch when pinned

A Lease binds (Repository, Worktree, Service, Port name) to one port and survives restarts. The Main worktree gets the declared port so existing habits keep working. Other Worktrees draw from the Pool first, for callbacks registered with OAuth or WebAuthn providers, then from the Range, each port checked with a real `bind`. A Pinned branch binds its Leases to the branch name so they survive the Worktree being recreated, and never expire. Other Leases expire after 30 days of inactivity by default.

## Considered Options

- A stateless hash of the worktree name (Worktrunk's `hash_port`, a hand-written setup script in one of our repositories): rejected, it collides silently.
- A block of ports per worktree: rejected, a block can collide with a Foreign process and readability comes later from Portless names.

## Consequences

- The Daemon serializes Lease allocation, so 2 agents starting at once never receive the same port.
- After a start, Procly checks that the process listens on its Lease and reports a Port mismatch otherwise, instead of a false healthy state.

## Exception for the Main worktree

Added with #7. When the declared port of the Main worktree is taken, by another Lease or by a Foreign process, its Lease falls back to the Range as above. The Main worktree then takes its declared port back on the first start where that port is free again, and the same happens when the declared port is edited. Since #177, an edited declaration follows the addendum below, which replaces this sentence for it. A Lease of the Main worktree can therefore change on its next start, and only toward its declared port. Every other Lease stays the same across restarts of the Service and of the Daemon. The behaviour of record is the unit test `the_main_worktree_gets_its_declared_port_back_once_it_is_free` in `crates/procly/src/daemon/lease.rs`.

### Addendum: the Lease follows an edited declaration

Added with #177. In the Main worktree, a Lease follows the declared port of `procly.toml` rather than keep the port it was first granted, and stores the declared port it was granted under, an optional `declared` key of `leases.json` that leaves the document at version 2, so that a release that does not know it still reads it.

- The move: while its Service does not run, a Lease of the Main worktree moves to its declared port on the next `up`, and on a read such as `env`, whenever that port is free, with the same `bind` probe as every other port. A running Service keeps its Lease until it stops, and `status` and `doctor` say that it differs from the declaration.
- The yield: the Lease of a stopped Service of the Main worktree that is no longer on its own declared port gives that port up to the Service that declares it now, which the Daemon logs. The Service that yielded has no Lease left, so its next start is a first grant, which falls back to the Pool or the Range when its own declared port is held. A running Service never yields, and the Pool and the Range never take a port from a Lease that yields, so that 2 Services whose declared ports were swapped each end on their own in either order.
- The refused move: when the recorded declaration differs from the current one and the new declared port is held by a running Service, a process, a container, a Lease of another Worktree or Repository or a Lease of a Pinned branch, a start fails with exit 125 naming the declared port, its holder and the command that frees it, `procly worktree release` in the Main worktree first wherever it helps, and the Lease keeps its port. A holder that does not answer, such as a connection left in `TIME_WAIT` by a Service that just stopped, holds the port too, and the message says that such a port frees itself within a minute before it says how to find a live holder. A start of a Process service or a Group checks this for the Service, or every Service its Group covers, before any Lease moves, and `restart` checks it before it stops the Service, counting the ports the Service or its Group holds as free since it stops them first. A Compose service does not rehearse: its Leases are granted as its container starts, and the Env file may move them before that. A read never fails, it shows the Lease where it stands.
- What stays a fallback: a declared port taken at the first grant under the same declaration keeps its fallback silently until it is free, as above. A Lease stored without `declared`, by procly 0.5.0 or by a later rewrite from it, has an unknown declaration: it takes the current one, moves when its declared port is free and otherwise keeps its port silently.

The behaviour of record is `grant_in` in `crates/procly/src/daemon/lease.rs`, its unit tests, and the tests of `crates/procly/tests/leases.rs` whose names start with `crossed_leases_`, `a_new_declared_port_`, `a_fallback_lease_written_by_0_5_0_` and `restart_refuses_`.

## Explicit port of a Pinned branch

Added with the `[pinned.<branch>.ports]` table. A Pinned branch may name an explicit port for a Port name of a Service. That port is its preferred one, the way the declared port is for the Main worktree: the Lease takes it whenever no other Lease and no Foreign process holds it, also when the Lease held another port before, so that editing it takes effect. When it is not available, the Lease keeps the port it has, and without one it falls back to the Pool, then the Range. A Pinned branch without an explicit port for a Port name goes to the Pool and the Range directly, it never takes the declared port. Its Leases never expire.

## The Main worktree never takes branch-scoped Leases

The Main worktree keeps Leases of its own whatever its branch, even when that branch is pinned, since its declared ports are already fixed and a Lease bound to the branch would keep them from it once the branch is checked out elsewhere. The container of a Compose service is shared by every Worktree, so it holds the Leases of the Main worktree and never those of a Pinned branch. The behaviour of record is `pinned_branch` in `crates/procly/src/daemon/environment.rs` and the unit test `a_pinned_branch_binds_its_lease_to_the_branch_and_prefers_its_explicit_port` in `crates/procly/src/daemon/lease.rs`.

## Isolated containers

Added with the Isolated mode. A Compose service set to `isolation = "isolated"` runs one container per Worktree, so its Leases are the ones of that Worktree, drawn as for a Process service: the declared port in the Main worktree, then the Pool and the Range, or the ports of a Pinned branch. The behaviour of record is `grant` in `crates/procly/src/daemon/environment.rs` and the test `each_worktree_gets_leases_of_its_own_and_the_main_worktree_keeps_the_declared_port` in `crates/procly/tests/isolated.rs`.
