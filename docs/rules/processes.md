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

- **ALWAYS** start the leader of a Service in a session of its own (`ProcessSession`, through `process_group::lead_session`), and treat the Service as every process of that session, not as its process group: stop it by waiting until the whole tree is gone, and read its listeners and its CPU time from every group of the tree.
  - A tool such as turbo runs each task in a process group of its own, and ends its own leader (`bun run dev`) a few milliseconds after `SIGTERM`, long before the tasks are stopped. A `SIGKILL` to the group at the leader's end kills the tool half way and orphans the tasks, and `lsof -g <leader group>` never sees their listeners.
  - The session is what survives: it is inherited by every descendant, whatever its process group, and it stays when the parent ends and init adopts the process, so the tree is found without following parents and without a periodic look that a task started and orphaned in between could slip through. `process_group::Tree` reads it from `/proc/<pid>/stat` on Linux and with `getsid` on macOS, whose `ps` has no session column.
  - Accepted limits, decided by the owner and not to be widened: a process that calls `setsid` (Node's `child_process.spawn(..., { detached: true })` does, through libuv) leaves the session, so it leaves the tree and survives `down` with its port, and `procly status` then shows it as Foreign. The leader is a session leader, so it can no longer call `setpgid` on itself, which fails with `EPERM`: a tool that treats that as fatal no longer starts, as interactive shells with job control may not, while `set -m` inside `run` works.
  - `stop_tree` sends `SIGTERM` to the leader's group, waits for the leader to end, sends `SIGTERM` to the other groups of the tree, waits for the whole tree, and sends `SIGKILL` to every group of the tree only at the stop timeout.
  - The Daemon owns the reaping of the leader of a Service it started, and `supervise` is the only place that reaps it. It waits for the end with `waitid(WEXITED | WNOWAIT)` (`process_group::ended`), which leaves the ended leader a zombie, and reaps it with `Child::wait` only after the tree is decided: gone, or killed at the stop timeout. Never `wait`, `try_wait` or drop the child before that, `tokio` reaps a child that is waited for or dropped and no other one. A zombie keeps its pid, so the ids of its process group and of its session cannot be given to another process, on macOS and on Linux (measured with a fork loop and with `ns_last_pid`). The tree holds the leader (`Known::hold`) until it is reaped: whatever listing shows in its session is the tree's, a child that a member started as it was asked to stop included, and macOS leaving a zombie out of the listing, since `getsid` fails on it, changes nothing.
  - The tree is the group of the leader only in 3 cases, and a task in a group of its own is then not stopped: a Service that an older Daemon started leads no session, and a Daemon that re-attaches it knows it by its process group, a Service whose leader ended while no Daemon ran is swept by the process group the record names, and a Daemon that cannot list the processes signals the group of the leader alone. Say so wherever the docs claim that nothing a Service started survives it.
  - Never signal a group that a listing taken just before did not show with a process of the tree in it, never trust the pid of a leader whose start differs from the one recorded, and never touch the Daemon's own group. When the processes cannot be listed, only the group of the leader is signalled, and only if the leader leads that group and has the start the tree saw (read at launch, or taken from the record), or the Daemon holds it unreaped, or it was found running with the start of its record, by identity, when the Daemon re-attached the Service, and has ended since. That last case is the group of the Service, signalled as it was before the Daemon followed sessions, with the window that a stranger took the id of the emptied group. A leader that ended while no Daemon supervised it, a leader that runs and cannot be identified, and a leader whose start was never seen get no signal at all. Without a listing the Daemon cannot tell that a group holds nothing but the ended leader it holds, since the zombie keeps the group from ever looking empty (`killpg(group, 0)` succeeds on Linux, and fails with `EPERM` on macOS). It does not reap the leader to find out, which would reopen the window: it sends `SIGKILL` to the group at once, a few times in a row, and reaps afterwards, so the members of that group get no time to stop on their own. Never signal a group after reaping its leader without a listing.
  - The id of a session or of a group is the pid of its leader, and the system hands that pid to no other process only while some process holds the id without a break. Once the session has been empty, an unrelated process can get the pid, call `setsid` and start children, and all of them are then its own. So never take a session or a group in through the id of the leader, and never signal one, unless the same listing shows the leader running with the start the tree has for it (a zombie not reaped yet counts, its pid is still its own), or the Daemon holds the leader unreaped. A leader the Daemon reaped is not held any more: the tree is then made of the processes it knew by pid and start. Once the leader is seen gone, or replaced by a process with another start, the tree keeps the processes it already knew from earlier listings by pid and start, and takes in what holds the session or the group besides them only while at least one of them is still there. When none is left the tree is over, and nothing that holds the id is the tree's. The one exception is a recorded leader that was found running with the start of its record, by identity, and that ended before any listing showed it: its process group, never its session, is the tree's, as the Service's was before sessions were followed, and the log says so when it takes processes in. A recorded leader nothing vouches for, or that a listing shows replaced, gets nothing: say in the log why, and signal nothing. What a process started while a re-attached Service stops, from a member that ended before the next listing, cannot be told from another tree's, and the log says so.
  - A Daemon that finds a Service running and cannot list the processes judges it by what it can tell: nothing answering on the Lease is silence and the Service is stopped, an answer from the group of the leader is a Service that runs, and an answer whose listener may be a task in a group of its own is unknown. Keep an unknown Service as it was, with its record, signal nothing, and say in the Daemon log that the processes could not be listed. Never turn "could not read" into "is silent".
  - An unknown Service is judged again at the first listing that succeeds (`Service::prove`), by the task that watches it once a second and by every `up` that finds it, whichever comes first. It then goes through the same judgement as a re-attach: its tree listens on the Lease and it becomes a Service like any other, or it does not and its tree is stopped, which signals only the processes of the tree. Never answer `reused` for a Service in doubt without that judgement: when the processes still cannot be listed, `up` says so in a warning.
  - A recorded tree is "unlisted" only until a listing was read, whether or not that listing found the leader. A recorded leader gone at its first listing was never seen running, so its tree is its process group if it was found running by identity and empty otherwise (see above), and the Service is handled as lost with its record.
  - A warning of `up` belongs in the document of the Service (`UpService.warnings`, then the `warnings` of `up --json`), not on stderr alone: an agent that calls the MCP tool never reads stderr, and `--json` keeps stderr empty on success.
  - Wait for a stopping tree by probing the groups known with `killpg(group, 0)` every 20 ms, and read the processes again only when those are all gone or every 500 ms, to find the groups that appeared since. A listing costs a `ps` of the whole system.
  - Share the listing: the Services of a Daemon that look at their tree within the same few milliseconds, or at their idleness within the same second, read the processes once. Nothing polls a tree that nobody asks about.
  - Example: `tree.kill()` sends `SIGKILL` to the groups of the processes of the session seen alive in a fresh listing, and repeats until none is left.

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
