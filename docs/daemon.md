# The Daemon across crashes and sessions

The first `procly` command starts the Daemon on demand, and the Daemon keeps running until it is stopped. This page tells what happens when it crashes, and how to have it start with your session instead.

## After a crash

Every Service runs in a process group of its own, so it outlives a Daemon killed by a crash or by `SIGKILL`. The Daemon records each running Service in `services.json`, in its state directory, with the process group and the start time of its leader. The next Daemon compares these records with the processes it finds before it answers any `up`, `down` or `status`, so that no Service ever runs twice:

| Found | What the new Daemon does | `procly status` |
| --- | --- | --- |
| The leader still runs, with the recorded start time, and its group listens on its Lease | Re-attaches it: `up` answers `reused` and `down` stops it | `service web healthy port 24000, re-attached after a restart of the Daemon` |
| The leader still runs but its group no longer listens on its Lease | Stops its process group | `service web ended: no longer listened on its Lease, port 24000, when the Daemon restarted, its process group was stopped` |
| The leader ended, but members of its group remain, such as a dev process forked by a shell | Stops them, they are orphans that would keep holding the Lease | `service web ended: its leader ended while no Daemon ran, the rest of process group 4242 held port 24000 of its Lease and was stopped` |
| Nothing of the group remains, or its pid now belongs to another process | Leaves the other process alone and forgets the Service | `service web ended: exit code unknown, a previous Daemon started it` |

The Daemon log names every Service it re-attached or stopped this way, with its process group and the port it held. In each case the Lease stays the same, so the next `up` of a Service that ended starts it again on the same port.

A re-attached Service is not a child of the new Daemon, which brings 3 limits:

- Its output is kept, without the restart showing in it. A Service writes into a pipe read by a writer of its own, `procly output-file`, which outlives the Daemon and appends to an output file in the `output` directory of the state directory. The next Daemon reads that file, so `procly logs` shows what the Service printed before, during and after the restart. While no Daemon runs, the writer keeps the file bounded: once it holds 4 MiB, it moves the last MiB, as much as the output buffer of a Service holds, into a `.prev` file next to it and empties the file. A Service printing with no Daemon running therefore takes at most about 5 MiB of disk, its most recent output, and what it printed before that is dropped. A running Daemon empties the file the same way at 1 MiB, once it has read it.
- Its exit code is unknown when it ends, and it is not restarted: its next `up` starts it again.
- It is stopped like any other Service, `SIGTERM` to its whole group then `SIGKILL`, but the Daemon looks at it every 100 ms rather than waiting for it.

A Service still starting when the Daemon crashed is re-attached only if it already listens on its Lease. Otherwise its process group is stopped, and its next `up` starts it again.

A `services.json` that cannot be read fails `up`, `down` and `status` with exit code 125 and its path, rather than starting Services that may still run. Fix or remove the file, the Services it recorded are then no longer known to Procly.

## Containers after a crash or a restart

The containers of Compose services outlive the Daemon too, whether it crashed or exited cleanly. The Daemon records each container it started or adopted in `containers.json`, in its state directory: the Repository, the compose project and service, the Worktrees that need it, and the end of its Grace period when one runs. The next Daemon compares these records with the containers Docker runs before it answers any `up`, `down`, `status` or `eject`:

| Found | What the new Daemon does |
| --- | --- |
| The container runs with the compose labels of its project and service | Re-attaches it with the Worktrees that need it: `up` answers `reused`, `down` tells how many other Worktrees still need it, `status` does not list it as foreign and `eject` stops it |
| It runs and its Grace period ended while no Daemon ran | Stops it before answering, never removes it, and keeps its volumes |
| It runs and its Grace period has not ended | Stops it at the end recorded by the previous Daemon, unless a Worktree needs it again before |
| It no longer runs, or runs without the labels of its compose service | Forgets it and never stops it, it is no longer the container Procly started |
| Docker does not answer | Re-attaches it as recorded, so that its counts are not lost while the engine is down |

A Worktree whose directory no longer exists, removed without `procly down`, stops counting when the Daemon reconciles and whenever `procly down` reads the count. A container no Worktree needs anymore then enters its Grace period.

Only what `docker compose` needs to reach the engine is recorded with a container: `PATH`, `HOME`, the `DOCKER_*` and `COMPOSE_*` variables, and the variables Procly injected. The rest of the environment of the command that started it is never written to disk.

A `containers.json` that cannot be read fails `up`, `down`, `status` and `eject` with exit code 125 and its path, as `services.json` does. Fix or remove the file, the containers it recorded are then no longer counted by Procly, and a `procly up` adopts a container still running with the labels of its compose service.

## Starting the Daemon with your session

```console
$ procly unit install
installed /Users/dev/Library/LaunchAgents/io.github.philippe-desplats.procly.plist
$ procly unit inspect
unit /Users/dev/Library/LaunchAgents/io.github.philippe-desplats.procly.plist
installed yes
starts with the session yes
daemon running yes
$ procly unit remove
removed /Users/dev/Library/LaunchAgents/io.github.philippe-desplats.procly.plist
```

The Session unit is a launchd agent on macOS, loaded with `launchctl bootstrap`, and a systemd user unit at `~/.config/systemd/user/procly.service` on Linux, enabled with `systemctl --user enable --now`. It runs the `procly` executable that installed it, with the state directory, the settings directory and the `PATH` of the shell it was installed from, and writes to the same `daemon.log`. Install it again after moving the executable or one of these directories.

The session manager restarts the Daemon only when it fails, so a Daemon already started on demand keeps serving, and leaves the Services running when the Daemon crashes, so that the restarted one re-attaches them.

Installing the unit again on macOS unloads it first, which stops the Daemon it started and its Services. On Linux the running Daemon keeps going, and the new unit applies from its next start.

`procly unit remove` stops the Daemon the unit started, which stops its Services, and the next command starts a Daemon on demand again.

With `--json`, the 3 commands print the `path` of the unit and their result: `installed` for `unit install`, `installed`, `starts_with_session` and `daemon_running` for `unit inspect`, and `removed` for `unit remove`, which is `false` when no unit was installed. See [JSON output](json.md).

```console
$ procly unit install --json
{"schema_version":1,"path":"/home/me/Library/LaunchAgents/io.github.philippe-desplats.procly.plist","installed":true}
$ procly unit inspect --json
{"schema_version":1,"path":"/home/me/Library/LaunchAgents/io.github.philippe-desplats.procly.plist","installed":true,"starts_with_session":true,"daemon_running":true}
$ procly unit remove --json
{"schema_version":1,"path":"/home/me/Library/LaunchAgents/io.github.philippe-desplats.procly.plist","removed":true}
```

## Foreign processes

`procly status` lists under each Worktree what runs in it without Procly having started it, so that an agent that bypassed Procly shows up:

- a process listening on TCP whose working directory is in the Worktree, outside the process groups of the Services and Jobs of Procly, as `foreign process <pid> <command> port <port>`
- a running container of a compose project whose directory is in the Worktree, unless Procly started or adopted it, as `foreign container <name> (compose project <project>, service <service>) port <port>`

`procly status --json` carries them in the `foreign` list of each Worktree that has some, with a `kind` of `process` or `container`, and leaves the list out of a Worktree that has none. Processes are found with `lsof`, which on macOS sees only the processes of your user, and containers with `docker ps`. Without `lsof` or a running Docker engine, the list is empty. Procly never stops a Foreign process, see [Containers started outside Procly](configuration.md#containers-started-outside-procly) for the ones in the way of a Compose service.
