# Procly

Procly is a local supervisor that coding agents call instead of starting dev processes and containers themselves. It exists so that parallel agents and parallel git worktrees never duplicate a process, a container or a port.

## Language

### Topology

**Repository**:
A git repository identified by its common git directory, shared by all of its worktrees.
_Avoid_: project, repo root

**Worktree**:
One checkout of a Repository on disk, the unit of isolation for services, leases and logical resources.
_Avoid_: checkout, workspace, branch

**Main worktree**:
The first worktree of a Repository, which keeps the ports and resource names declared in the config.
_Avoid_: primary, default worktree

**Worktree slug**:
The short, sanitized identifier of a Worktree used in derived names such as database names and cookie names.
_Avoid_: worktree id, suffix

**Daemon**:
The single per-user authority that owns every service, lease and logical resource across all Repositories and Worktrees.
_Avoid_: server, supervisor, backend

**Session unit**:
The launchd agent on macOS, or the systemd user unit on Linux, that starts the Daemon with the user's session and restarts it when it crashes.
_Avoid_: service unit, autostart, daemon service

### Services

**Service**:
A named unit of work declared for a Repository that Procly starts, health-checks and stops, either a Process service or a Compose service.
_Avoid_: server, app, process

**Process service**:
A Service that runs a host command, such as a dev server or a queue worker.
_Avoid_: host process, script

**Compose service**:
A Service that maps to a service of the Repository's existing compose file.
_Avoid_: container, docker service

**Group**:
A Service that runs several Services as one unit, such as a monorepo-wide dev command, and declares which Services it covers.
_Avoid_: meta-service, umbrella

**Job**:
A bounded, ad hoc command started without a config entry, deduplicated by its Fingerprint and ending with an exit code.
_Avoid_: temp, temporary process, task

**Fingerprint**:
The identity of a Job, made of its Worktree, working directory and command, used to return the running Job instead of starting a duplicate.
_Avoid_: hash, key

**Reuse**:
The outcome of a start request that found the Service or Job already running and returned it unchanged.
_Avoid_: attach, dedupe

**Foreign process**:
A dev process or container in a Worktree that was started outside Procly.
_Avoid_: rogue process, external server

**Adoption**:
Registering a Foreign process container as a Procly-owned Compose service because its compose labels match exactly.
_Avoid_: take-over, import

### Ports

**Port name**:
A named port declared by a Service, such as `http` or `test`, resolved to a number only through a Lease.
_Avoid_: port slot, port key

**Lease**:
The persistent binding of one Port name of one Service in one Worktree to one port number.
_Avoid_: allocation, reservation, assignment

**Pool**:
A short, fixed list of port numbers for a Port name, preferred over the Range because they are registered with third parties such as OAuth providers.
_Avoid_: reserved ports, whitelist

**Range**:
The general span of port numbers from which Leases are drawn once the declared port and the Pool are unavailable.
_Avoid_: port block

**Pinned branch**:
A branch whose Leases are bound to the branch name rather than to the Worktree, so they survive the Worktree being removed and recreated.
_Avoid_: reserved branch, fixed branch

**Port mismatch**:
The state of a Service whose process listens on a port other than the one its Lease granted.
_Avoid_: wrong port, port drift

### Isolation

**Isolation mode**:
How one Compose service is separated between Worktrees, one of Shared, Logical or Isolated.
_Avoid_: sharing mode, strategy

**Shared**:
The Isolation mode where all Worktrees use the same container and the same data.
_Avoid_: global

**Logical**:
The Isolation mode where all Worktrees use the same container, each with its own Logical resource inside it.
_Avoid_: soft isolation, namespaced

**Isolated**:
The Isolation mode where each Worktree runs its own container, under a compose project name of its own.
_Avoid_: container mode, dedicated

**Logical resource**:
A per-Worktree slice inside a Logical container, such as a Postgres database, a Redis database index or a MinIO bucket.
_Avoid_: tenant, namespace

**Driver**:
A declarative description of how to create, check and drop Logical resources for one technology, and how to build its connection URL.
_Avoid_: plugin, adapter, provider

**Seed**:
The initial content given to a new Logical resource, either nothing, a copy of a template resource, or the output of a command.
_Avoid_: fixture, bootstrap data

**Grace period**:
The delay a Shared or Logical container keeps running after its last Worktree stopped needing it.
_Avoid_: cooldown, linger

**Retention policy**:
The rule that decides what happens to an Isolated container's data when its Worktree is removed: keep, delete immediately or delete after a delay.
_Avoid_: cleanup policy, GC policy

### Configuration and environment

**Derived variable**:
An environment variable whose value is computed from templates over Leases and Worktree facts, such as an API URL embedding another Service's port.
_Avoid_: computed env, dynamic env

**Env file**:
The generated, git-excluded `.env.procly` that mirrors the variables Procly injects, for tools Procly does not launch.
_Avoid_: dotenv, local env

**Worktree init**:
The one-time, idempotent preparation of a new Worktree: copying untracked files from the Main worktree and running its setup commands.
_Avoid_: bootstrap, setup script, provisioning

### Agent integration and reversibility

**Guard**:
An agent hook that blocks a known dev-server command and answers with the equivalent Procly command.
_Avoid_: interceptor, blocker

**Rules block**:
The Procly instructions written into a project's agent instruction file, only on explicit request.
_Avoid_: managed rule, injected rules

**Managed block**:
Any text Procly writes into a file it does not own, delimited by markers and recorded with the fingerprint of its content.
_Avoid_: marker section, snippet

**Off**:
The per-Repository switch that makes Guards and Rules blocks stand aside without removing anything.
_Avoid_: disable, pause

**Eject**:
The removal of everything Procly wrote into a Repository, mechanically for intact Managed blocks and through a generated agent prompt for edited ones.
_Avoid_: uninstall, teardown

## Relationships

- A **Repository** has one **Main worktree** and zero or more other **Worktrees**.
- A **Repository** declares many **Services**, and each **Service** runs at most once per **Worktree**.
- A **Group** covers one or more **Services**, and a start request for a covered **Service** while its **Group** runs ends in **Reuse**.
- A **Service** declares zero or more **Port names**, and each **Port name** gets one **Lease** per **Worktree**, or per **Pinned branch**.
- A **Lease** is drawn from the declared port in the **Main worktree**, then from the **Pool**, then from the **Range**.
- A **Compose service** has exactly one **Isolation mode** per **Repository**.
- A **Logical** container holds one or more **Logical resources** per **Worktree**, created by its **Driver** and filled by its **Seed**.
- A **Shared** or **Logical** container is reference-counted by **Worktrees** and stops after its **Grace period**.
- **Derived variables** are injected into every **Process service** and mirrored in the **Env file**.
- **Eject** removes every **Managed block**, including **Rules blocks** and **Guards**.

## Flagged ambiguities

- "isolated" and "container" were both used for one container per Worktree. Resolved: the mode is **Isolated**, and "container mode" is retired.
- "shared" first covered both a plain shared container and a shared container with per-Worktree databases. Resolved: they are 2 distinct modes, **Shared** and **Logical**.
- "server" in Portly meant any supervised command. Resolved: Procly says **Service** for declared work and **Job** for ad hoc commands, and "server" is retired.
- "temporary job" in Portly was a background command with a deadline. Resolved: Procly calls it a **Job**, deduplicated by its **Fingerprint**, which Portly's did not do.
- "workspace" names the Cargo workspace in build files and Rust rules, never a Worktree. "supervisor" describes what Procly is, the running process is the **Daemon**.
- "reserved ports" was used both for OAuth callback ports and for ports kept by `develop`. Resolved: the first is a **Pool**, the second is a **Pinned branch**.
