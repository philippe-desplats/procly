import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { afterEach, describe, expect, it, vi } from "vitest";

import recordedGc from "./fixtures/gc.json";
import { createOrphanScanStore, orphanCount } from "./orphan-scan";
import type { Response } from "./protocol/Response";

const gc: Response = recordedGc as Response;

function reportOf(response: Response) {
  if (response.response !== "gc") {
    throw new Error("not a Gc answer");
  }
  return response;
}

describe("the Orphan scan", () => {
  afterEach(() => {
    clearMocks();
  });

  it("knows nothing until a dry run answered", () => {
    mockIPC(() => gc);

    const store = createOrphanScanStore();

    expect(store.get()).toEqual({ kind: "idle" });
    expect(orphanCount(store.get())).toBeNull();
  });

  it("keeps the Orphans of the last dry run and counts them", async () => {
    mockIPC((command) => (command === "list_orphans" ? gc : Promise.reject(new Error(command))));
    const store = createOrphanScanStore();

    await store.run();

    expect(store.get()).toEqual({ kind: "report", report: reportOf(gc) });
    expect(orphanCount(store.get())).toBe(6);
  });

  it("keeps the previous report on show while a new dry run is on its way", async () => {
    let release: (() => void) | undefined;
    let first = true;
    mockIPC(() => {
      if (first) {
        first = false;
        return gc;
      }
      return new Promise((resolve) => {
        release = () => {
          resolve({ ...reportOf(gc), orphans: [] });
        };
      });
    });
    const store = createOrphanScanStore();
    await store.run();

    const second = store.run();

    expect(store.get()).toEqual({ kind: "running", report: reportOf(gc) });
    expect(orphanCount(store.get())).toBe(6);
    await vi.waitFor(() => {
      expect(release).toBeDefined();
    });
    release?.();
    await second;
    expect(orphanCount(store.get())).toBe(0);
  });

  it("says why there is no report when the Daemon did not answer, and counts nothing", async () => {
    mockIPC(() => Promise.reject(new Error("the daemon did not answer")));
    const store = createOrphanScanStore();

    await store.run();

    expect(store.get().kind).toBe("failure");
    expect(orphanCount(store.get())).toBeNull();
  });

  it("lets the newest dry run win over one that answers late", async () => {
    const answers: ((value: unknown) => void)[] = [];
    mockIPC(
      () =>
        new Promise((resolve) => {
          answers.push(resolve);
        }),
    );
    const store = createOrphanScanStore();

    const older = store.run();
    const newer = store.run();
    await vi.waitFor(() => {
      expect(answers).toHaveLength(2);
    });
    answers[1]?.({ ...reportOf(gc), orphans: [] });
    await newer;
    answers[0]?.(gc);
    await older;

    expect(orphanCount(store.get())).toBe(0);
  });

  it("tells its listeners when the state changes", async () => {
    mockIPC(() => gc);
    const store = createOrphanScanStore();
    let heard = 0;
    const stop = store.subscribe(() => {
      heard += 1;
    });

    await store.run();
    stop();
    await store.run();

    expect(heard).toBe(2);
  });
});
