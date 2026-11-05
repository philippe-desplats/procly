# Tauri v2

How the desktop app is written. The official pages are vendored in [`docs/reference/tauri-v2/`](../reference/README.md), read them there rather than recalling Tauri v1, whose APIs differ.

## Role of the app

- **NEVER** put supervision logic in the desktop app, every Tauri command sends one `Request` through the `procly-client` crate and returns the `Response` unchanged.
  - ADR 0009: the CLI is the contract, `procly mcp` and the app are logic-free clients, so behaviour can never diverge between surfaces. Reshaping the answer in Rust would start a second rendering of the protocol, the frontend draws it instead. The one exception is the "Needs you" list, derived in Rust next to the unchanged `Response`, see the rule below.
  - Example:
    ```rust
    #[tauri::command]
    async fn status(cwd: PathBuf, all: bool, client: State<'_, DaemonClient>) -> Result<Response, ClientError> {
        client.send(&Request::Status { cwd, all }).await
    }
    ```

- **ALWAYS** derive "Needs you" and hold the Seen set in Rust, once, and send the frontend the derived list next to the unchanged `Response`.
  - The native menu bar menu is built in Rust and must agree with the window, so a second copy of the rule in TypeScript would drift. It is the one derivation the app makes in Rust, and it is presentation, not supervision: it decides nothing in the Daemon.
  - A failure is keyed by (Worktree path, Service, `started_at`), or by the `id` of a Job, so a Service that fails again after a new start is not taken for the one already seen.
  - An end is routine only when it is `stopped`, `idle` or `exited` with code 0, and every other end is a failure, so an end kind the app does not know yet counts as one. The list of routine ends is the allowlist, never a list of failures.
  - The Seen set is `seen.json` in `app_data_dir()`, written atomically. Each answer to `Status` drops the keys it holds no failure for, while an answer that is not a status leaves the set alone, so that a Daemon that stops does not bring every failure back.
  - Example: `needs_you(&report, &seen) -> Vec<NeedsYou>`, called on each answer of the poller, its result emitted with the `Response` and the Seen keys, so the frontend dims a Seen row without deciding anything.

- **NEVER** poll the Daemon from the frontend or from a command, every refresh goes through the one poller task the app spawns at startup.
  - A JavaScript timer in a hidden webview is throttled by WebKit and App Nap, and the menu must stay current with the window closed. Each `Status` spawns 4 processes plus 1 per Repository, so 2 pollers double that cost.
  - The poller keeps 1 `Status` in flight, waits at least 4 times the last one's duration, ticks every 2 s with the window in front, 10 s visible, 30 s closed, and pauses while the Mac sleeps. Without a Daemon it only calls `connect`, every 5 s visible and 30 s otherwise.
  - Example: the `refresh` command behind ⌘R sends a message to the poller asking for an immediate tick, it never calls `client.send` itself.

- **ALWAYS** reach `doctor` and `daemon stop` by running `procly doctor --json` and `procly daemon stop --json`, never through a socket request.
  - ADR 0009: these are the only 2 verbs the app reaches through the CLI. The checks of the installation must still report a Daemon that does not run, and stopping a Daemon of another protocol version cannot go through a socket it would refuse.
  - Example: the `restart_daemon` command runs `procly daemon stop --json`, then calls `connect_or_start`, and only after the developer confirmed that every running Service stops.

- **NEVER** send `Request::Up` from the app, start a Service again only through `Request::Restart`, and only when its `ServiceStatus` says `restartable`.
  - ADR 0014: a Service runs with the environment of the request that started it, and an app opened from the Dock only has the `PATH` of launchd. `Restart` replays the environment the Daemon holds from the last `up`, so the app never gives an environment of its own to start anything.
  - Without a held environment, a re-attached Service or a Daemon that restarted, the inspector offers the command to copy instead, `(cd <path> && procly restart <service>)`. A Service a Group covers is restarted from the row of its Group.
  - Example: `async fn restart(service: String, cwd: PathBuf, client: State<'_, DaemonClient>) -> Result<Response, ClientError>` sends `Request::Restart { service, cwd }`, and the button is drawn only when `restartable` is true.

- **ALWAYS** send `gc --apply` from the app as `Request::Gc { apply: true, only }`, with `only` listing the Orphans of the dry run the developer confirmed, never as a bare apply.
  - `gc --apply` deletes the Isolated containers that are Orphans and their volumes, from what it finds when it runs, so a bare apply could delete data the sheet never showed. The Daemon deletes only the listed Orphans that still are, and reports the others as not deleted with the reason.
  - ADR 0009: `only` is the one request field the app sends that the CLI does not offer, and `procly gc --apply` keeps deleting every Orphan it finds.
  - Example: the sheet keeps the `orphans` of the last `Gc { apply: false }`, and its Delete button sends them back unchanged as `only`.

- **NEVER** let the app guess the `procly` executable, pass the sidecar it embeds explicitly to `connect_or_start` and to every CLI command.
  - Inside a Tauri bundle `current_exe()` is the app, and starting it with `daemon` would launch a second app instead of the Daemon. A `procly` found on the `PATH` may be another release, whose Daemon the app could not talk to.
  - Example: `DaemonClient::connect_or_start(socket_path, &procly_exe)`, where `procly_exe` is `Procly.app/Contents/MacOS/procly`.

- **NEVER** start the Daemon implicitly, refresh with `connect` and call `connect_or_start` only from an action the developer took.
  - A Daemon the developer stopped must stay stopped, and a Session unit keeps it running anyway when one is installed.
  - Example: a refresh that finds no socket shows the "Daemon not running" state with the `doctor` report, it does not start one.

- **NEVER** open a TCP or HTTP listener from the app to talk to the Daemon.
  - ADR 0003: the Daemon listens on a `0600` Unix socket only, and a loopback HTTP API is reachable from any browser tab.
  - Example: the app holds a `DaemonClient` connected to the same socket path the CLI uses.

## Layout

- **ALWAYS** place the app under `apps/desktop/`, with its Rust crate in `apps/desktop/src-tauri/` added to the root workspace members.
  - The app then shares the workspace lockfile, lints and toolchain, and reuses the `procly-client` crate directly.
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
  - Example: `async fn logs(name: String, cwd: PathBuf, client: State<'_, DaemonClient>) -> Result<Response, ClientError>`.

- **ALWAYS** stream the output of a Service or a Job to the frontend through a `tauri::ipc::Channel`, and keep events for small notifications.
  - Channels are ordered and built for throughput, the event system evaluates JavaScript per message and does not scale to log streams.
  - Only the item shown in the inspector is followed, through `Logs { follow: true }`. Changing the selection or hiding the window drops the connection, which cancels the follow in the Daemon. A Compose service has no follow and keeps a snapshot with a Refresh.
  - Example: `async fn follow(name: String, cwd: PathBuf, on_output: Channel<Response>) -> Result<(), ClientError>`, forwarding the backlog, each `Output` and the final `LogsEnded` unchanged.

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

- **ALWAYS** test the frontend against `mockIPC` from `@tauri-apps/api/mocks`, and test the Rust side through the `procly-client` crate.
  - The commands hold no logic, so the frontend tests check rendering and the client tests check the protocol, without a running webview.
  - Example: `mockIPC((cmd, args) => cmd === 'restart' ? { response: 'up', service: 'web', outcome: 'started', port: 5173 } : undefined)`.
