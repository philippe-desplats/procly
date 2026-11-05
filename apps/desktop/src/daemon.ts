import { invoke } from "@tauri-apps/api/core";

import type { ClientFailure } from "./protocol/ClientFailure";
import type { ClientFailureKind } from "./protocol/ClientFailureKind";
import type { Response } from "./protocol/Response";
import type { StatusReport } from "./protocol/StatusReport";

/** What the window knows of the Daemon: the last answer of its poller, or why there is none. */
export type DaemonSnapshot =
  | { kind: "loading" }
  | { kind: "status"; report: StatusReport }
  | { kind: "no-daemon" }
  | { kind: "failure"; message: string };

/** The last answer of the Daemon, read through `useSyncExternalStore`. */
export interface DaemonStore {
  subscribe(listener: () => void): () => void;
  getSnapshot(): DaemonSnapshot;
  /** Asks the Rust side for one `Status { all: true }`. Calls made while one is in flight share it. */
  refresh(): Promise<void>;
}

/** A command failure as the Rust side sends it: a `ClientFailure`, or the same shape with the kind `task` for a broken thread. */
type Failure = Omit<ClientFailure, "kind"> & { kind: ClientFailureKind | "task" };

const NO_DAEMON: ClientFailureKind = "unreachable";

function isFailure(error: unknown): error is Failure {
  return (
    typeof error === "object" &&
    error !== null &&
    "kind" in error &&
    typeof error.kind === "string" &&
    "message" in error &&
    typeof error.message === "string"
  );
}

function fromResponse(response: Response): DaemonSnapshot {
  switch (response.response) {
    case "status":
      return { kind: "status", report: response };
    case "error":
      return { kind: "failure", message: response.message };
    default:
      return { kind: "failure", message: `unexpected answer from the Daemon: ${response.response}` };
  }
}

function fromError(error: unknown): DaemonSnapshot {
  if (isFailure(error)) {
    return error.kind === NO_DAEMON ? { kind: "no-daemon" } : { kind: "failure", message: error.message };
  }
  return { kind: "failure", message: String(error) };
}

export function createDaemonStore(): DaemonStore {
  let snapshot: DaemonSnapshot = { kind: "loading" };
  let inFlight: Promise<void> | null = null;
  const listeners = new Set<() => void>();

  function publish(next: DaemonSnapshot): void {
    snapshot = next;
    for (const listener of listeners) {
      listener();
    }
  }

  async function load(): Promise<void> {
    try {
      publish(fromResponse(await invoke<Response>("status")));
    } catch (error) {
      publish(fromError(error));
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
    refresh() {
      inFlight ??= load().finally(() => {
        inFlight = null;
      });
      return inFlight;
    },
  };
}
