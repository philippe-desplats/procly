# Persistent state as versioned JSON documents replaced atomically

The Daemon persists its state as JSON documents in the state directory, one per concern, starting with `leases.json` for the Leases. Each document carries a `version` field and is replaced atomically: written to a temporary file next to it, flushed to disk, renamed over it, then the directory is flushed. The Daemon is the only writer, it holds the lock of the state directory (ADR 0001), so a document needs no locking of its own, and Leases are granted one request at a time in memory before anything is written. A document is read on the first request that needs it, so an unreadable one fails that request with exit code 125 and its path, instead of keeping the Daemon from starting.

## Considered Options

- SQLite through `rusqlite`: rejected for v0.1. Its concurrency control serves several writers, and Procly has one. It compiles a bundled C library, which slows every cold build and complicates the release binaries of the distribution ticket, and its file cannot be read or fixed by hand.
- An embedded key-value store such as `redb`: rejected, it brings the same opaque file for a few hundred records at most, with no query the Daemon needs.
- One document for the whole state: rejected, every change would rewrite every concern, and the tickets that add Services, reference counts or Managed block fingerprints would all edit the same type.

## Consequences

- A crash leaves either the previous document or the new one, never a partial write.
- A document written by a newer Procly is refused with its path rather than misread, an older version is read as it is.
- The state stays readable and fixable with a text editor, which matters for a tool whose output agents and developers read.
- If the state outgrows rewriting a document on each change, moving to SQLite replaces the store module, the documents give its initial import.
