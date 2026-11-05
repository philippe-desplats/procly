import { describe, expect, it } from "vitest";

import type { JobEnd } from "./protocol/JobEnd";
import type { ServiceEnd } from "./protocol/ServiceEnd";
import { jobEndWord, serviceEndWord } from "./state";

describe("the state words", () => {
  it("draws a clean stop as ended and a crash as a failure", () => {
    expect(serviceEndWord({ kind: "stopped" }).tone).toBe("ended");
    expect(serviceEndWord({ kind: "exited", code: 0 }).tone).toBe("ended");
    expect(serviceEndWord({ kind: "exited", code: 3 })).toEqual({ word: "exited 3", tone: "fail" });
    expect(jobEndWord({ kind: "timed_out", timeout_ms: 1000 }).tone).toBe("fail");
  });

  it("counts an end kind it does not know as a failure instead of breaking the window", () => {
    const future = { kind: "evicted" } as unknown as ServiceEnd;
    const futureJob = { kind: "evicted" } as unknown as JobEnd;

    expect(serviceEndWord(future)).toEqual({ word: "unknown", tone: "fail" });
    expect(jobEndWord(futureJob)).toEqual({ word: "unknown", tone: "fail" });
  });
});
