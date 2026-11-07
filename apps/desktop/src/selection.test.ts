import { describe, expect, it } from "vitest";

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

describe("the Foreign process the Doctor shows", () => {
  it("is none when the Status reports no Foreign process", () => {
    expect(foreignToShow(report(worktree("/p/work/acme/web")))).toBeNull();
  });

  it("is the first Foreign process of the Status, named by its Worktree and its pid", () => {
    const found = foreignToShow(
      report(
        worktree("/p/work/acme/web"),
        worktree("/p/work/acme/web-fix", {
          foreign: [{ kind: "process", pid: 4812, command: "node server.js", ports: [3000] }],
        }),
      ),
    );

    expect(found).toEqual({ kind: "foreign", worktree: "/p/work/acme/web-fix", process: "pid:4812" });
  });

  it("prefers the one that holds a port a Service of its own Worktree has, over an earlier one", () => {
    const found = foreignToShow(
      report(
        worktree("/p/work/acme/web", {
          foreign: [{ kind: "process", pid: 100, command: "node other.js", ports: [3000] }],
        }),
        worktree("/p/work/acme/web-fix", {
          services: [
            {
              service: "web",
              port: 24081,
              reattached: false,
              started_at: 0,
              restartable: false,
              state: "ended",
              end: { kind: "port_mismatch", leased: 24081, actual: 10189 },
            },
          ],
          foreign: [{ kind: "process", pid: 4812, command: "node server.js", ports: [24081] }],
        }),
      ),
    );

    expect(found).toEqual({ kind: "foreign", worktree: "/p/work/acme/web-fix", process: "pid:4812" });
  });

  it("names a container by its name", () => {
    const found = foreignToShow(
      report(
        worktree("/p/work/acme/web-fix", {
          foreign: [
            { kind: "container", name: "legacy-db", project: "legacy", service: "db", ports: [5432] },
          ],
        }),
      ),
    );

    expect(found).toEqual({
      kind: "foreign",
      worktree: "/p/work/acme/web-fix",
      process: "container:legacy-db",
    });
  });
});
