# Exit codes

Every `procly` command exits with one of these codes. They are stable: an agent may branch on them without parsing the output, and a change to this table is a breaking change of the CLI (ADR 0009).

| Code | Meaning | Commands |
| --- | --- | --- |
| 0 | Success. For `up`, the Service is healthy, whether it was `started` or `reused`. For `worktree init`, the Worktree is ready, also when there was nothing to do. | all |
| 2 | Usage error: an unknown command, a missing argument or an invalid flag value. | all |
| 3 | The Service ended before it became healthy: it exited, was killed by a signal, was stopped by `procly down` or for being idle, or kept crashing until Procly gave up restarting it. A Compose service whose container fails to start exits with 125 instead, with the error of `docker compose` on stderr. For a Service that `up` was asked for through `needs`, the one that failed is named. Its recent output follows the error on stderr. | `up` |
| 4 | Port mismatch: the process of the Service listened on another port than its Lease, and its process tree was stopped. The error on stderr names both ports, its recent output follows. | `up` |
| 124 | Timeout. For `wait`, the Job reached its timeout and its process tree was killed. For `up`, the Service, or one it needs, did not become healthy within its health timeout and its process tree was stopped, or its container was stopped for a Compose service. The code is the one `timeout(1)` uses. For `worktree init`, a setup command ran past its time limit and its process tree was killed. | `wait`, `up`, `worktree init` |
| 125 | Procly itself failed: the Daemon could not be reached, the configuration is invalid (including a Derived variable that cannot be rendered), the Service or one it needs is not declared, the needs loop back to each other, a state document such as `leases.json` is unreadable, the Range has no free port left, the directory is not in a git Repository, the Job is unknown, `docker compose` v2 is unavailable for a Compose service, the compose file lacks the service or does not publish its Lease, `docker compose` fails to start the container, the Driver of a Compose service is missing or invalid, or a Logical resource cannot be created or its Seed fails (the error says whether the resource was dropped). The reason is on stderr. | all |

`worktree init` exits with the code of the first setup command that failed, 128 + N when it was killed by signal N, and 124 when it ran past its 30 minutes limit. Setup commands after it are not run, and it runs again on the next `worktree init`. As for `wait`, those codes can collide with the ones above, so read them as the command's when `worktree init` reports no Procly error on stderr.

`agent install`, `off` and `on` exit with 0, also when a Managed block was already in place or was left as it is because you edited it, and with 125 when Procly itself fails: a settings file that is not JSON, an instruction file outside the Worktree, a state document that is unreadable, or a directory that is not in a git Repository. `guard` always exits with 0. It denies a command through the JSON it prints and lets the others through, see [Steering agents to Procly](agents.md).

`wait` otherwise exits with the Job's own exit code, and with 128 + N for a Job killed by signal N, as a shell does. Those codes can collide with the ones above, so read them as the Job's when `wait` reports no Procly error on stderr.

The code for a conflict with a Foreign process is reserved for the ticket that introduces it.
