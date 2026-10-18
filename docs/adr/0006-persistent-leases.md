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
