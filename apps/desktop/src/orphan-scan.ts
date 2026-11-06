import { useSyncExternalStore } from "react";

import { failureMessage } from "./daemon";
import { listOrphans } from "./orphans";
import type { GcReport } from "./protocol/GcReport";

/** What the window knows of the last dry run of `Gc`: nothing yet, its answer, or why there is none. */
export type OrphanScan =
  | { kind: "idle" }
  /** A dry run is on its way, `report` is the last answer while there is one, so that a rescan does not blank the list. */
  | { kind: "running"; report: GcReport | null }
  | { kind: "report"; report: GcReport }
  | { kind: "failure"; message: string };

/**
 * The Orphans of the last dry run, shared by the Orphans screen and the sidebar. The window never scans on a timer:
 * it scans when the app launches, on Refresh and when the Orphans screen opens, so the count may be stale in between.
 */
export interface OrphanScanStore {
  subscribe(listener: () => void): () => void;
  get(): OrphanScan;
  /** Asks the Daemon for a dry run. When several are on their way, the newest answer is the one that is kept. */
  run(): Promise<void>;
}

/** The report a scan holds, the last answer while a new run is on its way. */
export function reportOfScan(scan: OrphanScan): GcReport | null {
  switch (scan.kind) {
    case "running":
      return scan.report;
    case "report":
      return scan.report;
    case "idle":
    case "failure":
      return null;
    default: {
      const unhandled: never = scan;
      throw new Error(`unhandled scan: ${JSON.stringify(unhandled)}`);
    }
  }
}

/** How many Orphans the last dry run found, or `null` while no dry run answered. */
export function orphanCount(scan: OrphanScan): number | null {
  return reportOfScan(scan)?.orphans.length ?? null;
}

export function createOrphanScanStore(): OrphanScanStore {
  let scan: OrphanScan = { kind: "idle" };
  let newest = 0;
  const listeners = new Set<() => void>();

  function set(next: OrphanScan): void {
    scan = next;
    for (const listener of listeners) {
      listener();
    }
  }

  return {
    subscribe(listener) {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
    get: () => scan,
    run() {
      newest += 1;
      const mine = newest;
      set({ kind: "running", report: reportOfScan(scan) });
      return listOrphans().then(
        (report) => {
          if (mine === newest) {
            set({ kind: "report", report });
          }
        },
        (reason: unknown) => {
          if (mine === newest) {
            set({ kind: "failure", message: failureMessage(reason) });
          }
        },
      );
    },
  };
}

/** The last dry run, read through `useSyncExternalStore`. */
export function useOrphanScan(store: OrphanScanStore): OrphanScan {
  return useSyncExternalStore(store.subscribe, store.get);
}
