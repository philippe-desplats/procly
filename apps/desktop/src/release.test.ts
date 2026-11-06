import { describe, expect, it } from "vitest";

import { endedIn, endedWords } from "./release";
import type { WorktreeStatus } from "./protocol/WorktreeStatus";

const exited = { kind: "exited", code: 1 } as const;

describe("the ended items a release forgets", () => {
  it("counts the ended Jobs and the ended Services, not the ones that run or restart", () => {
    const worktree: Pick<WorktreeStatus, "jobs" | "services"> = {
      jobs: [
        { id: "a", command: "make", cwd: "/p", state: "running" },
        { id: "b", command: "make", cwd: "/p", state: "ended", end: exited },
      ],
      services: [
        { service: "web", port: 1, reattached: false, started_at: 0, restartable: true, state: "healthy" },
        { service: "api", port: 2, reattached: false, started_at: 0, restartable: true, state: "restarting" },
        { service: "db", port: 3, reattached: false, started_at: 0, restartable: true, state: "starting" },
        {
          service: "old",
          port: null,
          reattached: false,
          started_at: 0,
          restartable: true,
          state: "ended",
          end: exited,
        },
      ],
    };

    expect(endedIn(worktree)).toEqual({ jobs: 1, services: 1 });
  });

  it("words each kind alone, in the singular for one, and nothing when none is left", () => {
    expect(endedWords({ jobs: 2, services: 1 })).toBe("2 ended Jobs and 1 ended Service");
    expect(endedWords({ jobs: 1, services: 0 })).toBe("1 ended Job");
    expect(endedWords({ jobs: 0, services: 3 })).toBe("3 ended Services");
    expect(endedWords({ jobs: 0, services: 0 })).toBe("");
  });
});
