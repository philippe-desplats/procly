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
  | { kind: "protocol-mismatch"; daemon: number; app: number; at: number }
  | { kind: "failure"; message: string; at: number };

/**
 * A command failure as the Rust side sends it: a `ClientFailure`, or the same shape with another kind: `task` for a
 * broken thread, `sidecar` when the embedded `procly` cannot run, `daemon_stop` when `procly daemon stop` failed.
 */
export type Failure =
  | (Omit<ClientFailure, "kind"> & { kind: ClientFailureKind | "task" | "sidecar" | "daemon_stop" })
  | ProtocolMismatch;

/** A Daemon that answers in another protocol version, with the version it speaks and the one of this app. */
export interface ProtocolMismatch {
  kind: "protocol_mismatch";
  message: string;
  daemon_protocol: number;
  app_protocol: number;
}

/** What a rejected command says, whether the Rust side sent a `Failure` or something else broke. */
export function failureMessage(error: unknown): string {
  if (typeof error === "string") {
    return error;
  }
  if (
    typeof error === "object" &&
    error !== null &&
    "message" in error &&
    typeof error.message === "string"
  ) {
    return error.message;
  }
  return "unknown error";
}

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
  /** Starts the Daemon with the `procly` the app embeds. Only the developer's click calls it. Rejects with why it failed. */
  startDaemon(): Promise<void>;
  /** Stops the Daemon of another version, then starts the embedded one. Only a confirmed sheet calls it. Rejects with why it failed. */
  restartDaemon(): Promise<void>;
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
  if (failure.kind === "protocol_mismatch") {
    return {
      kind: "protocol-mismatch",
      daemon: failure.daemon_protocol,
      app: failure.app_protocol,
      at: update.at_ms,
    };
  }
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
    startDaemon: () => invoke("start_daemon"),
    restartDaemon: () => invoke("restart_daemon"),
  };
}
