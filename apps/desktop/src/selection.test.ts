import { describe, expect, it } from "vitest";

import type { ForeignProcess } from "./protocol/ForeignProcess";
import type { ServiceStatus } from "./protocol/ServiceStatus";
import type { StatusReport } from "./protocol/StatusReport";
import type { WorktreeStatus } from "./protocol/WorktreeStatus";
import { foreignToShow } from "./selection";

function worktree(path: string, rest: Partial<WorktreeStatus> = {}): WorktreeStatus {
  return {
    path,
    slug: path.split("/").at(-1) ?? path,
    main: false,
    listed: true,
    services: [],
    jobs: [],
    ...rest,
  };
}

function report(...worktrees: WorktreeStatus[]): StatusReport {
  return {
    repositories: [{ git_common_dir: "/p/work/acme/web/.git", worktrees }],
    jobs_outside_repositories: [],
  };
}

function service(name: string, port: number): ServiceStatus {
  return {
    service: name,
    port,
    reattached: false,
    started_at: 0,
    restartable: false,
    state: "ended",
    end: { kind: "port_mismatch", leased: port, actual: 10189 },
  };
}

function node(pid: number, port: number): ForeignProcess {
  return { kind: "process", pid, command: "node server.js", ports: [port] };
}

/** The message of a failed `leased_ports` check naming one Lease per `[service, port, worktree]`. */
function message(...leases: [string, number, string][]): string {
  return leases
    .map(
      ([name, port, path]) =>
        `lease ${name}.http port ${String(port)} of ${path} is held by process 1 node, which Procly did not start`,
    )
    .join("\n");
}

const WEB = "/p/work/acme/web";
const FIX = "/p/work/acme/web-fix";

describe("the Foreign process the Doctor shows", () => {
  it("is none when the Status reports no Foreign process", () => {
    expect(foreignToShow(report(worktree(WEB)), message(["web", 24081, FIX]))).toBeNull();
  });

  it("is a Foreign process that holds a port a Service of its own Worktree has, named by Worktree and pid", () => {
    const found = foreignToShow(
      report(
        worktree(WEB),
        worktree(FIX, { services: [service("web", 24081)], foreign: [node(4812, 24081)] }),
      ),
      message(["web", 24081, FIX]),
    );

    expect(found).toEqual({ kind: "foreign", worktree: FIX, process: "pid:4812" });
  });

  it("is none for a Foreign process that holds no port of a Service of its Worktree, however many there are", () => {
    // A Port mismatch with no occupant, and a process of another Worktree holding no Lease: neither is what the check names.
    const found = foreignToShow(
      report(
        worktree(WEB, { foreign: [node(100, 3000)] }),
        worktree(FIX, { services: [service("web", 24081)], foreign: [node(4812, 3000)] }),
      ),
      message(["web", 24081, FIX]),
    );

    expect(found).toBeNull();
  });

  it("is none for a Service port held by a Foreign process of another Worktree", () => {
    const found = foreignToShow(
      report(
        worktree(WEB, { foreign: [node(100, 24081)] }),
        worktree(FIX, { services: [service("web", 24081)] }),
      ),
      message(["web", 24081, FIX]),
    );

    expect(found).toBeNull();
  });

  it("prefers, among several, the one on the port of the first Lease the check names", () => {
    const worktrees = report(
      worktree(WEB, { services: [service("api", 24000)], foreign: [node(100, 24000)] }),
      worktree(FIX, { services: [service("web", 24081)], foreign: [node(4812, 24081)] }),
    );

    // The Status lists web first, the check names the Lease of web-fix first.
    expect(foreignToShow(worktrees, message(["web", 24081, FIX], ["api", 24000, WEB]))).toEqual({
      kind: "foreign",
      worktree: FIX,
      process: "pid:4812",
    });
    expect(foreignToShow(worktrees, message(["api", 24000, WEB], ["web", 24081, FIX]))).toEqual({
      kind: "foreign",
      worktree: WEB,
      process: "pid:100",
    });
  });

  it("takes the first of the Status when the check names none of the candidates", () => {
    const found = foreignToShow(
      report(worktree(FIX, { services: [service("web", 24081)], foreign: [node(4812, 24081)] })),
      "service web of /p/work/acme/web-fix listens on port 10189 instead of its Lease, port 24081 (Port mismatch)",
    );

    expect(found).toEqual({ kind: "foreign", worktree: FIX, process: "pid:4812" });
  });

  it("names a container by its name", () => {
    const found = foreignToShow(
      report(
        worktree(FIX, {
          services: [service("db", 5432)],
          foreign: [
            { kind: "container", name: "legacy-db", project: "legacy", service: "db", ports: [5432] },
          ],
        }),
      ),
      message(["db", 5432, FIX]),
    );

    expect(found).toEqual({ kind: "foreign", worktree: FIX, process: "container:legacy-db" });
  });
});
