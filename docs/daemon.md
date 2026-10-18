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

- Its output went to the previous Daemon and is lost. A Service that prints after the crash writes to a closed pipe and usually dies of it, the Daemon then sees it ended.
- Its exit code is unknown when it ends, and it is not restarted: its next `up` starts it again.
- It is stopped like any other Service, `SIGTERM` to its whole group then `SIGKILL`, but the Daemon looks at it every 100 ms rather than waiting for it.

A Service still starting when the Daemon crashed is re-attached only if it already listens on its Lease. Otherwise its process group is stopped, and its next `up` starts it again.

A `services.json` that cannot be read fails `up`, `down` and `status` with exit code 125 and its path, rather than starting Services that may still run. Fix or remove the file, the Services it recorded are then no longer known to Procly.

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

## Foreign processes

`procly status` lists under each Worktree what runs in it without Procly having started it, so that an agent that bypassed Procly shows up:

- a process listening on TCP whose working directory is in the Worktree, outside the process groups of the Services and Jobs of Procly, as `foreign process <pid> <command> port <port>`
- a running container of a compose project whose directory is in the Worktree, unless Procly started or adopted it, as `foreign container <name> (compose project <project>, service <service>) port <port>`

`procly status --json` carries them in the `foreign` list of each Worktree that has some, with a `kind` of `process` or `container`, and leaves the list out of a Worktree that has none. Processes are found with `lsof`, which on macOS sees only the processes of your user, and containers with `docker ps`. Without `lsof` or a running Docker engine, the list is empty. Procly never stops a Foreign process, see [Containers started outside Procly](configuration.md#containers-started-outside-procly) for the ones in the way of a Compose service.
