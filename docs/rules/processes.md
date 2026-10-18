# Processes and PTYs

How Procly starts, watches and stops the processes it supervises. The API references are [`process-wrap`](../reference/rust/process-wrap.md) and [`portable-pty`](../reference/rust/portable-pty.md). Their rustdoc is rendered from the Linux build, so Windows-only items (`JobObject`, `CreationFlags`, ConPTY) are documented on docs.rs only.

## Spawning

- **ALWAYS** depend on `process-wrap` and `portable-pty` at the versions vendored in `docs/reference/`, and bump the vendored docs in the same change as the version.
  - The rules and examples here are checked against those exact APIs, a different version silently invalidates them.
  - Example: `process-wrap = { version = "10.0.1", features = ["tokio1"] }` and `portable-pty = "0.9.0"` in `[workspace.dependencies]`, then `make docs` after any bump.

- **ALWAYS** spawn a supervised process through `process_wrap::tokio::CommandWrap` with a new process group on Unix and a Job Object on Windows, never through a bare `Command::spawn`.
  - Dev servers fork whole trees (`pnpm` starts `node`, which starts `esbuild`). Killing only the pid leaves the grandchildren holding the port, which is the exact duplicate Procly exists to prevent.
  - Example:
    ```rust
    use process_wrap::tokio::CommandWrap;

    let mut command = CommandWrap::with_new(program, |c| {
        c.args(args).current_dir(worktree).envs(injected_env);
    });
    #[cfg(unix)]
    command.wrap(process_wrap::tokio::ProcessGroup::leader());
    #[cfg(windows)]
    command.wrap(process_wrap::tokio::JobObject);
    let child = command.spawn()?;
    ```

- **ALWAYS** pass the Lease ports and the Derived variables through the child's environment, never by writing a file the repository owns.
  - ADR 0007: the repository's `.env` stays untouched, and the git-excluded Env file is a mirror for tools Procly does not launch.
  - Example: `c.env("PORT", lease.port.to_string()).env("API_URL", derived.render("api_url")?)`.

- **ALWAYS** set the working directory of a spawned process to its Worktree explicitly.
  - The Daemon is global (ADR 0001), its own working directory is meaningless for any one Repository.
  - Example: `c.current_dir(&worktree.path)`.

## Stopping

- **ALWAYS** stop a process by signalling its whole group, `SIGTERM` first, then `SIGKILL` once the stop timeout expires (not the Grace period of the glossary, which applies to containers).
  - A dev server needs a moment to flush and release its port, a hung one must still go. `ChildWrapper::signal` on a `ProcessGroupChild` reaches every member of the group.
  - Example:
    ```rust
    #[cfg(unix)]
    async fn stop_tree(child: &mut dyn ChildWrapper, stop_timeout: Duration) -> io::Result<ExitStatus> {
        child.signal(libc::SIGTERM)?;
        if let Ok(status) = tokio::time::timeout(stop_timeout, child.wait()).await {
            return status;
        }
        child.start_kill()?;
        child.wait().await
    }
    ```

- **ALWAYS** kill the whole tree of a Job that reaches its timeout, and report exit code 124.
  - Issue #1, user story 12, asks for it. 124 is the code `timeout(1)` uses, so agents already know it, and a runaway build must never outlive its Job.
  - Example: `procly wait <job>` exits 124 after the Job's tree was killed on timeout.

- **NEVER** drop a child without waiting for it.
  - An unreaped child stays a zombie, and its pid can be confused with a live process in status output. `ProcessGroupChild` reaps every member of the group when waited.
  - Example: every stop path ends with `child.wait().await`, including the error paths.

## Output

- **ALWAYS** capture the output of a Service or Job into a bounded ring buffer, never into an unbounded `Vec`.
  - A dev server can run for days, and agents only read the recent output (issue #1, user story 13).
  - Example: a fixed byte capacity per Service, the oldest lines dropped first once it is full.

- **ALWAYS** start with pipes, and run a process under a PTY only when it needs a terminal to behave (colors, progress bars, interactive prompts).
  - A PTY merges stdout and stderr, adds terminal escape codes and needs blocking IO on dedicated threads. Pipes are cheaper and keep the 2 streams apart.
  - Example: a Service runs under pipes unless its configuration opts into a PTY, and the ticket that adds PTY support defines that option.

## PTY

- **ALWAYS** drop the slave side of a `portable-pty` pair right after spawning the command.
  - The reader on the master only reaches end of file once every slave handle is closed, a kept slave makes the output reader hang forever after the child exits.
  - Example:
    ```rust
    let size = PtySize { rows: 24, cols: 120, pixel_width: 0, pixel_height: 0 };
    let pair = native_pty_system().openpty(size).map_err(PtyError::Open)?;
    let child = pair.slave.spawn_command(command).map_err(PtyError::Spawn)?;
    drop(pair.slave);
    let reader = pair.master.try_clone_reader().map_err(PtyError::Open)?;
    ```

- **ALWAYS** read and write a PTY on a blocking thread, never on an async task.
  - `try_clone_reader` and `take_writer` return blocking `std::io` handles. See `rust.md`, Async.
  - Example: `tokio::task::spawn_blocking(move || pump(reader, ring))`.

- **ALWAYS** stop a PTY child through its process group, found with `MasterPty::process_group_leader`.
  - The PTY child leads its own session, so its descendants share its group and a signal to the pid alone leaves them running.
  - Example: `if let Some(pgid) = pair.master.process_group_leader() { signal_group(pgid, SIGTERM)?; }`.

## Windows

- **ALWAYS** keep platform-specific code behind `#[cfg(unix)]` and `#[cfg(windows)]` at the spawn and stop boundary only, with one shared interface above it.
  - ADR 0002 keeps a Windows port possible, and the Windows support of `process-wrap` is not yet verified in practice. Confining the difference to one module keeps that verification to one place.
  - Example: a `fn stop_tree(child: &mut dyn ChildWrapper, stop_timeout: Duration)` with 2 `cfg` implementations, called identically everywhere else.
