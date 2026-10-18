# Rust for the daemon and CLI, Tauri for the desktop app

Procly is written in Rust, and its later desktop app uses Tauri. Performance and memory safety do not separate Go and Rust for an IO-bound daemon. The desktop app and an eventual Windows port do: Tauri is the most mature desktop stack and shares the daemon's language, `portable-pty` handles ConPTY in production inside WezTerm, and `process-wrap` covers Unix process groups and Windows Job Objects.

## Consequences

- The Go code from Portly is a reference only, about 1 500 lines of mechanics to rewrite.
- The repository ships a Rust and Tauri knowledge pack (project rules and reference docs) so that agents work idiomatically from the first ticket.
- The Windows support of `process-wrap` is not yet verified in practice.
