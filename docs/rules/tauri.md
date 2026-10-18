# Tauri v2

How the desktop app is written. The official pages are vendored in [`docs/reference/tauri-v2/`](../reference/README.md), read them there rather than recalling Tauri v1, whose APIs differ.

## Role of the app

- **NEVER** put supervision logic in the desktop app, every Tauri command forwards to the Daemon socket and returns its answer.
  - ADR 0009: the CLI is the contract, `procly mcp` and the app are logic-free clients, so behaviour can never diverge between surfaces.
  - Example:
    ```rust
    #[tauri::command]
    async fn up(service: String, client: State<'_, DaemonClient>) -> Result<UpReport, ClientError> {
        client.up(&service).await
    }
    ```

- **NEVER** open a TCP or HTTP listener from the app to talk to the Daemon.
  - ADR 0003: the Daemon listens on a `0600` Unix socket only, and a loopback HTTP API is reachable from any browser tab.
  - Example: the app holds a `DaemonClient` connected to the same socket path the CLI uses.

## Layout

- **ALWAYS** place the app under `apps/desktop/`, with its Rust crate in `apps/desktop/src-tauri/` added to the root workspace members.
  - The app then shares the workspace lockfile, lints and toolchain, and reuses the socket client crate directly.
  - Example: `members = ["crates/*", "apps/desktop/src-tauri"]` in the root `Cargo.toml`.

- **ALWAYS** start from the Tauri v2 project structure: `src-tauri/tauri.conf.json`, `src-tauri/capabilities/`, and a `lib.rs` holding the `run()` function that `main.rs` calls.
  - The v2 layout keeps the app buildable for every target, and the generated schemas in `src-tauri/gen/schemas/` validate the capability files.
  - Example:
    ```
    apps/desktop/src-tauri/
    ├── Cargo.toml
    ├── tauri.conf.json
    ├── capabilities/main-window.json
    └── src/
        ├── lib.rs      # pub fn run(), the tauri::Builder and its commands
        └── main.rs     # calls procly_desktop_lib::run()
    ```

## Commands and IPC

- **ALWAYS** return `Result<T, E>` from an `async` command, with an error type that implements `serde::Serialize`.
  - An async command may only borrow arguments such as `&str` or `State<'_, T>` when it returns a `Result`, and a serializable error reaches the frontend as a rejected promise instead of a panic.
  - Example: `async fn logs(service: String, lines: usize) -> Result<Vec<String>, ClientError>`.

- **ALWAYS** stream Service output to the frontend through a `tauri::ipc::Channel`, and keep events for small notifications.
  - Channels are ordered and built for throughput, the event system evaluates JavaScript per message and does not scale to log streams.
  - Example: `async fn follow(service: String, on_line: Channel<LogLine>) -> Result<(), ClientError>`.

- **ALWAYS** import the frontend API from the v2 module paths.
  - The v1 paths no longer exist and a recalled v1 snippet fails at build time.
  - Example: `import { invoke, Channel } from '@tauri-apps/api/core'`, never `from '@tauri-apps/api/tauri'`.

- **ALWAYS** spawn background work from the app with `tauri::async_runtime::spawn`, never by creating a second Tokio runtime.
  - Tauri owns the runtime, a nested runtime panics or splits the tasks across 2 schedulers.
  - Example: in `setup`, `tauri::async_runtime::spawn(watch_daemon(app.handle().clone()))`.

## State

- **ALWAYS** register shared state with `app.manage()` and read it through `State<'_, T>`, never through a global.
  - Managed state is injected per command and dropped with the app, a `static` outlives the runtime and hides the dependency. The mutex choice follows `rust.md`, Async.
  - Example: `app.manage(DaemonClient::connect(socket_path)?);`.

## Security

- **ALWAYS** grant each window an explicit list of permissions in its own file under `src-tauri/capabilities/`, starting from nothing.
  - A window present in several capabilities merges all their permissions, and broad defaults expose APIs the app never calls.
  - Example:
    ```json
    {
      "$schema": "../gen/schemas/desktop-schema.json",
      "identifier": "main-window",
      "windows": ["main"],
      "permissions": ["core:event:default", "core:window:allow-set-title"]
    }
    ```

- **ALWAYS** set a restrictive Content Security Policy in `tauri.conf.json`.
  - The webview renders Service output, which is untrusted text, and a CSP limits what injected markup can load or run.
  - Example: `"security": { "csp": "default-src 'self'; connect-src ipc: http://ipc.localhost" }`.

## Tests

- **ALWAYS** test the frontend against `mockIPC` from `@tauri-apps/api/mocks`, and test the Rust side through the socket client crate.
  - The commands hold no logic, so the frontend tests check rendering and the client tests check the protocol, without a running webview.
  - Example: `mockIPC((cmd, args) => cmd === 'up' ? { state: 'reused', port: 5173 } : undefined)`.
