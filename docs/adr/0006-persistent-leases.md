# Persistent port leases keyed by worktree, or by branch when pinned

A Lease binds (Repository, Worktree, Service, Port name) to one port and survives restarts. The Main worktree gets the declared port so existing habits keep working. Other Worktrees draw from the Pool first, for callbacks registered with OAuth or WebAuthn providers, then from the Range, each port checked with a real `bind`. A Pinned branch binds its Leases to the branch name so they survive the Worktree being recreated, and never expire. Other Leases expire after 30 days of inactivity by default.

## Considered Options

- A stateless hash of the worktree name (Worktrunk's `hash_port`, a hand-written setup script in one of our repositories): rejected, it collides silently.
- A block of ports per worktree: rejected, a block can collide with a Foreign process and readability comes later from Portless names.

## Consequences

- The Daemon serializes Lease allocation, so 2 agents starting at once never receive the same port.
- After a start, Procly checks that the process listens on its Lease and reports a Port mismatch otherwise, instead of a false healthy state.

## Exception for the Main worktree

Added with #7. When the declared port of the Main worktree is taken, by another Lease or by a Foreign process, its Lease falls back to the Range as above. The Main worktree then takes its declared port back on the first start where that port is free again, and the same happens when the declared port is edited. A Lease of the Main worktree can therefore change on its next start, and only toward its declared port. Every other Lease stays the same across restarts of the Service and of the Daemon. The behaviour of record is the unit test `the_main_worktree_gets_its_declared_port_back_once_it_is_free` in `crates/procly/src/daemon/lease.rs`.

## Explicit port of a Pinned branch

Added with the `[pinned.<branch>.ports]` table. A Pinned branch may name an explicit port for a Port name of a Service. That port is its preferred one, the way the declared port is for the Main worktree: the Lease takes it whenever no other Lease and no Foreign process holds it, also when the Lease held another port before, so that editing it takes effect. When it is not available, the Lease keeps the port it has, and without one it falls back to the Pool, then the Range. A Pinned branch without an explicit port for a Port name goes to the Pool and the Range directly, it never takes the declared port. Its Leases never expire.

## The Main worktree never takes branch-scoped Leases

The Main worktree keeps Leases of its own whatever its branch, even when that branch is pinned, since its declared ports are already fixed and a Lease bound to the branch would keep them from it once the branch is checked out elsewhere. The container of a Compose service is shared by every Worktree, so it holds the Leases of the Main worktree and never those of a Pinned branch. The behaviour of record is `pinned_branch` in `crates/procly/src/daemon/environment.rs` and the unit test `a_pinned_branch_binds_its_lease_to_the_branch_and_prefers_its_explicit_port` in `crates/procly/src/daemon/lease.rs`.

## Isolated containers

Added with the Isolated mode. A Compose service set to `isolation = "isolated"` runs one container per Worktree, so its Leases are the ones of that Worktree, drawn as for a Process service: the declared port in the Main worktree, then the Pool and the Range, or the ports of a Pinned branch. The behaviour of record is `grant` in `crates/procly/src/daemon/environment.rs` and the test `each_worktree_gets_leases_of_its_own_and_the_main_worktree_keeps_the_declared_port` in `crates/procly/tests/isolated.rs`.
