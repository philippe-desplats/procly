import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import type { ClientFailure } from "./protocol/ClientFailure";
import type { ClientFailureKind } from "./protocol/ClientFailureKind";
import type { Response } from "./protocol/Response";
import type { StatusReport } from "./protocol/StatusReport";

/** What the window knows of the Daemon: the last answer of the poller, or why there is none. `at` is when it was read. */
export type DaemonSnapshot =
  | { kind: "loading" }
  | { kind: "status"; report: StatusReport; at: number }
  | { kind: "no-daemon"; at: number }
  | { kind: "failure"; message: string; at: number };

/** A command failure as the Rust side sends it: a `ClientFailure`, or the same shape with the kind `task` for a broken thread. */
export type Failure = Omit<ClientFailure, "kind"> & { kind: ClientFailureKind | "task" };

/** One tick of the poller, as the Rust side emits it: the time it was read, and the `Response` unchanged or why there is none. */
export type Update = { at_ms: number } & ({ response: Response } | { failure: Failure });

/** The event the poller emits on each tick. */
export const DAEMON_UPDATE = "daemon-update";

/** The last answer of the Daemon, read through `useSyncExternalStore`. The window never polls, the poller of the app does. */
export interface DaemonStore {
  subscribe(listener: () => void): () => void;
  getSnapshot(): DaemonSnapshot;
  /** Starts following the poller, and returns what stops it. */
  start(): () => void;
  /** Asks the poller for a tick now. It changes nothing in the Daemon, and the answer comes back as any other tick. */
  refresh(): Promise<void>;
}

const NO_DAEMON: ClientFailureKind = "unreachable";

function fromResponse(response: Response, at: number): DaemonSnapshot {
  switch (response.response) {
    case "status":
      return { kind: "status", report: response, at };
    case "error":
      return { kind: "failure", message: response.message, at };
    default:
      return { kind: "failure", message: `unexpected answer from the Daemon: ${response.response}`, at };
  }
}

function fromUpdate(update: Update): DaemonSnapshot {
  if ("response" in update) {
    return fromResponse(update.response, update.at_ms);
  }
  const { failure } = update;
  return failure.kind === NO_DAEMON
    ? { kind: "no-daemon", at: update.at_ms }
    : { kind: "failure", message: failure.message, at: update.at_ms };
}

export function createDaemonStore(): DaemonStore {
  let snapshot: DaemonSnapshot = { kind: "loading" };
  const listeners = new Set<() => void>();

  function publish(next: DaemonSnapshot): void {
    snapshot = next;
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
    getSnapshot: () => snapshot,
    start() {
      let stopped = false;
      let unlisten: (() => void) | null = null;
      void (async () => {
        // Listen first, so that no tick falls between the last answer and the first event.
        const off = await listen<Update>(DAEMON_UPDATE, (event) => {
          if (!stopped) {
            publish(fromUpdate(event.payload));
          }
        });
        if (stopped) {
          off();
          return;
        }
        unlisten = off;
        const last = await invoke<Update | null>("last_update");
        // A tick that arrived meanwhile is newer than the one the poller kept.
        if (!stopped && last !== null && snapshot.kind === "loading") {
          publish(fromUpdate(last));
        }
      })();
      return () => {
        stopped = true;
        unlisten?.();
      };
    },
    refresh: () => invoke("refresh"),
  };
}
