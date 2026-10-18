# Rust

How Rust code is written in this repository. Process and PTY handling has its own file, [`processes.md`](processes.md). The exact API of the crates named in ADR 0002 is vendored in [`docs/reference/`](../reference/README.md), read it there rather than from memory.

## Crate layout

- **ALWAYS** add a crate under `crates/<name>/` as a member of the root virtual workspace, and inherit its package metadata and lints from the workspace.
  - One `Cargo.lock`, one lint policy and one toolchain for every crate keep the CI and every agent on the same settings.
  - Example:
    ```toml
    [package]
    name = "procly-daemon"
    version.workspace = true
    edition.workspace = true
    rust-version.workspace = true

    [lints]
    workspace = true
    ```

- **ALWAYS** declare a third-party dependency once in `[workspace.dependencies]` of the root `Cargo.toml`, and reference it with `.workspace = true` from the crate.
  - Two crates pinning 2 versions of the same dependency compile it twice and drift apart silently.
  - Example: `clap.workspace = true` in `crates/procly/Cargo.toml`, with `clap = { version = "4.6", features = ["derive"] }` at the root.

- **NEVER** split a new library crate out before a second consumer needs it.
  - A crate boundary is a public interface to maintain. The daemon, the CLI, `procly mcp` and the desktop app (ADR 0009) justify a shared crate once 2 of them exist, not before.
  - Example: the first daemon ticket adds modules under `crates/procly/src/`, and the socket client moves to its own crate only when the desktop app needs it too.

- **ALWAYS** name types, modules and functions after the terms of `GLOSSARY.md`.
  - The glossary is the ubiquitous language, a synonym in code forces every reader to translate.
  - Example: `struct Lease`, `enum IsolationMode { Shared, Logical, Isolated }`, never `PortReservation` or `Mode::Separate`.

## Errors

- **ALWAYS** return a typed error enum built with `thiserror` from library code, and keep `anyhow` for the outermost layer of a binary only.
  - The CLI promises stable exit codes (issue #1, user story 16), and only a typed error can be matched to one. An `anyhow::Error` erases the variant the exit code depends on.
  - Example:
    ```rust
    #[derive(Debug, thiserror::Error)]
    pub enum UpError {
        #[error("service {service} did not become healthy within {timeout:?}")]
        HealthTimeout { service: String, timeout: Duration },
        #[error("service {service} listens on {actual}, its lease is {leased}")]
        PortMismatch { service: String, leased: u16, actual: u16 },
    }
    ```

- **NEVER** panic on a recoverable failure outside tests: no `unwrap()`, and an `expect()` only for a broken invariant, with a message that states it.
  - The Daemon is long-lived and supervises other people's work, a panic takes every Service down with it. `clippy::unwrap_used` is set to warn and the lint step denies warnings.
  - Example: `let leases = state.leases.lock().expect("lease table mutex is never poisoned, no code panics while holding it");`

- **ALWAYS** add context at the boundary where an IO error happens, naming the path, the command or the socket involved.
  - `No such file or directory (os error 2)` with no path costs an agent a whole investigation.
  - Example: `#[error("cannot read {path}")] ReadConfig { path: PathBuf, #[source] source: io::Error }`.

## Async

- **ALWAYS** use Tokio as the async runtime, with the multi-thread scheduler in the Daemon.
  - Tauri v2 runs on Tokio (`tauri::async_runtime`) and `process-wrap` ships a Tokio frontend, so a second runtime would only add a bridge.
  - Example: `#[tokio::main] async fn main() -> anyhow::Result<()>` in the daemon entry point, `#[tokio::test]` in its tests.

- **NEVER** run blocking work on an async task, move it to `tokio::task::spawn_blocking` or a dedicated thread.
  - A blocking call parks a runtime worker, and enough of them freeze the socket for every agent. `portable-pty` readers and writers are blocking `std::io` types.
  - Example: `tokio::task::spawn_blocking(move || copy_pty_output(reader, buffer))`.

- **NEVER** hold a `std::sync::Mutex` guard across an `.await`, release it first or use `tokio::sync::Mutex` for that one case.
  - The guard is not `Send`, and holding it across a suspension point deadlocks or fails to compile. A short critical section with a std mutex is otherwise the preferred choice.
  - Example: `let port = { leases.lock().expect("...").allocate(key)? }; probe(port).await?;`

## Unsafe

- **NEVER** write `unsafe` without `#[expect(unsafe_code, reason = "...")]` on the smallest enclosing item and a `// SAFETY:` comment stating why the contract holds.
  - `unsafe_code` is denied workspace-wide. `process-wrap` and `portable-pty` already wrap the `setpgid`, `setsid` and PTY syscalls, so an exception should be rare and visible in review.
  - Example:
    ```rust
    #[expect(unsafe_code, reason = "no safe wrapper for getpgrp in the dependencies")]
    fn current_pgid() -> libc::pid_t {
        // SAFETY: getpgrp takes no arguments, cannot fail and touches no memory.
        unsafe { libc::getpgrp() }
    }
    ```

## Tests

- **ALWAYS** test a binary through the binary itself, from `crates/<crate>/tests/`, using `env!("CARGO_BIN_EXE_<name>")`.
  - The CLI is the contract (ADR 0009). A test that calls internal functions passes while the command an agent actually runs is broken.
  - Example: `crates/procly/tests/cli.rs` runs `procly --version` and asserts on its stdout and exit status.

- **ALWAYS** test pure logic (parsing, port arithmetic, template rendering) with unit tests in a `#[cfg(test)] mod tests` at the bottom of its module.
  - Pure logic tested in place stays fast and close to the code. Everything touching a process, a socket or the filesystem goes through an integration test instead.
  - Example: `Range` parsing and Derived variable rendering get unit tests, `procly up` gets an integration test.

- **ALWAYS** mark a test that needs Docker by a name starting with `docker_` and a first line `if !require_docker() { return; }`, from `tests/common/mod.rs`.
  - A machine without `docker compose` v2 skips these slow tests and prints why, so the suite stays green there.
  - Name every container, network and volume of the test after a per-test prefix, remove them when the test ends, and never touch anything else in the shared engine.
  - Example: `docker_2_worktrees_share_one_container_on_one_port` in `crates/procly/tests/compose.rs`.

- **ALWAYS** isolate a test that touches the filesystem or a socket in its own temporary directory, and never read or write the user's real state directory.
  - Tests run in parallel and on a developer's machine where a real Daemon may be running.
  - Example: the code under test takes its state directory as a parameter, and the test passes a `tempfile::TempDir`.

## Checks

- **ALWAYS** run `make check` before pushing, it runs `cargo fmt --check`, Clippy and rustdoc with warnings denied, then the tests.
  - The CI runs the same `make` targets on macOS and Linux, so a green local `make check` predicts a green CI.
  - Example: `make check`, or `make fmt` first to apply the formatting.

- **ALWAYS** silence a lint with `#[expect(<lint>, reason = "...")]`, never with a bare `#[allow]`.
  - Clippy `pedantic` is on and some of its lints will be wrong for a given line. `expect` records why, and fails the build once the exception is no longer needed.
  - Example: `#[expect(clippy::cast_possible_truncation, reason = "ports are validated to fit in u16 by Range::parse")]`.
