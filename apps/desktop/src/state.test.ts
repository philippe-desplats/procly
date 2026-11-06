import { describe, expect, it } from "vitest";

import type { JobEnd } from "./protocol/JobEnd";
import type { JobStatus } from "./protocol/JobStatus";
import type { ServiceEnd } from "./protocol/ServiceEnd";
import type { ServiceStatus } from "./protocol/ServiceStatus";
import { foreignWord, jobEndWord, jobWord, serviceEndWord, serviceWord } from "./state";

const SERVICE = { service: "web", port: 3000, reattached: false, started_at: 0, restartable: true };
const JOB = { id: "j1", command: "make test", cwd: "/p/work/acme/web" };

describe("the state words", () => {
  it("draw an end as a failure only when Rust says it is one, whatever the end", () => {
    expect(serviceEndWord({ kind: "exited", code: 3 }, true)).toEqual({
      word: "exited 3",
      tone: "fail",
      glyph: "failure",
    });
    expect(serviceEndWord({ kind: "exited", code: 3 }, false)).toEqual({
      word: "exited 3",
      tone: "ended",
      glyph: "ended",
    });
    expect(serviceEndWord({ kind: "stopped" }, false).tone).toBe("ended");
    expect(jobEndWord({ kind: "timed_out", timeout_ms: 1000 }, true).tone).toBe("fail");
  });

  it("name an end kind this app does not know without breaking the window", () => {
    const future = { kind: "evicted" } as unknown as ServiceEnd;
    const futureJob = { kind: "evicted" } as unknown as JobEnd;

    expect(serviceEndWord(future, true)).toEqual({ word: "unknown", tone: "fail", glyph: "failure" });
    expect(jobEndWord(futureJob, true)).toEqual({ word: "unknown", tone: "fail", glyph: "failure" });
  });
});

describe("the glyph of a state", () => {
  const glyphOfService = (service: ServiceStatus, failed = false) => serviceWord(service, failed).glyph;
  const glyphOfJob = (job: JobStatus, failed = false) => jobWord(job, failed).glyph;

  it("gives a Service, a Job and a Foreign process each a shape of their own", () => {
    const glyphs = [
      glyphOfService({ ...SERVICE, state: "healthy" }),
      glyphOfService({ ...SERVICE, state: "restarting" }),
      glyphOfService({ ...SERVICE, state: "ended", end: { kind: "stopped" } }),
      glyphOfService({ ...SERVICE, state: "ended", end: { kind: "exited", code: 1 } }, true),
      glyphOfJob({ ...JOB, state: "running" }),
      foreignWord().glyph,
    ];

    expect(glyphs).toEqual(["healthy", "busy", "ended", "failure", "job", "foreign"]);
  });

  it("draws a Job that ended like a Service that ended, a failure only when Rust says so", () => {
    const ended: JobStatus = { ...JOB, state: "ended", end: { kind: "exited", code: 1 } };

    expect(glyphOfJob(ended, true)).toBe("failure");
    expect(glyphOfJob(ended, false)).toBe("ended");
  });

  it("shows a Service that is starting as busy, as the mockup does", () => {
    expect(glyphOfService({ ...SERVICE, state: "starting" })).toBe("busy");
  });

  it("draws a Foreign process amber, a warning and not a failure", () => {
    expect(foreignWord()).toEqual({ word: "started outside Procly", tone: "warn", glyph: "foreign" });
  });
});
