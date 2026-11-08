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

const WEB = "/p/work/acme/web";
const FIX = "/p/work/acme/web-fix";

describe("the Foreign process the Doctor shows", () => {
  it("is none when the Status reports no Foreign process", () => {
    expect(foreignToShow(report(worktree(WEB)))).toBeNull();
  });

  it("is a Foreign process that holds a port a Service of its own Worktree has, named by Worktree and pid", () => {
    const found = foreignToShow(
      report(
        worktree(WEB),
        worktree(FIX, { services: [service("web", 24081)], foreign: [node(4812, 24081)] }),
      ),
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
    );

    expect(found).toBeNull();
  });

  it("is none for a Service port held by a Foreign process of another Worktree", () => {
    const found = foreignToShow(
      report(
        worktree(WEB, { foreign: [node(100, 24081)] }),
        worktree(FIX, { services: [service("web", 24081)] }),
      ),
    );

    expect(found).toBeNull();
  });

  it("takes, among several, the first in Status order", () => {
    const first = report(
      worktree(WEB, { services: [service("api", 24000)], foreign: [node(100, 24000)] }),
      worktree(FIX, { services: [service("web", 24081)], foreign: [node(4812, 24081)] }),
    );
    const swapped = report(
      worktree(FIX, { services: [service("web", 24081)], foreign: [node(4812, 24081)] }),
      worktree(WEB, { services: [service("api", 24000)], foreign: [node(100, 24000)] }),
    );

    expect(foreignToShow(first)).toEqual({ kind: "foreign", worktree: WEB, process: "pid:100" });
    expect(foreignToShow(swapped)).toEqual({ kind: "foreign", worktree: FIX, process: "pid:4812" });
  });

  it("skips, within a Worktree, a Foreign process on no port of its Services for the next one that is", () => {
    const found = foreignToShow(
      report(
        worktree(FIX, { services: [service("web", 24081)], foreign: [node(100, 3000), node(4812, 24081)] }),
      ),
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
    );

    expect(found).toEqual({ kind: "foreign", worktree: FIX, process: "container:legacy-db" });
  });
});
