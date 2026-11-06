import { describe, expect, it } from "vitest";

import type { JobEnd } from "./protocol/JobEnd";
import type { ServiceEnd } from "./protocol/ServiceEnd";
import { jobEndWord, serviceEndWord } from "./state";

describe("the state words", () => {
  it("draw an end as a failure only when Rust says it is one, whatever the end", () => {
    expect(serviceEndWord({ kind: "exited", code: 3 }, true)).toEqual({ word: "exited 3", tone: "fail" });
    expect(serviceEndWord({ kind: "exited", code: 3 }, false)).toEqual({ word: "exited 3", tone: "ended" });
    expect(serviceEndWord({ kind: "stopped" }, false).tone).toBe("ended");
    expect(jobEndWord({ kind: "timed_out", timeout_ms: 1000 }, true).tone).toBe("fail");
  });

  it("name an end kind this app does not know without breaking the window", () => {
    const future = { kind: "evicted" } as unknown as ServiceEnd;
    const futureJob = { kind: "evicted" } as unknown as JobEnd;

    expect(serviceEndWord(future, true)).toEqual({ word: "unknown", tone: "fail" });
    expect(jobEndWord(futureJob, true)).toEqual({ word: "unknown", tone: "fail" });
  });
});
