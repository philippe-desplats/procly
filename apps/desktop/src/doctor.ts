import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useEffect, useRef, useSyncExternalStore } from "react";

import { failureMessage } from "./daemon";
import type { Check } from "./protocol/Check";
import type { CheckStatus } from "./protocol/CheckStatus";

/**
 * The document `procly doctor --json` prints, as the Rust side hands it over unchanged. It is the one document of the
 * CLI the app reads: the checks of the installation live in the CLI and must still report a Daemon that does not run
 * (ADR 0009), so it is no `Response` of the protocol.
 */
export interface DoctorDocument {
  schema_version: number;
  result: "healthy" | "problems";
  checks: Check[];
  /** What `procly doctor --agent` would hand to a coding agent for the failed checks, `null` when none failed. */
  prompt: string | null;
  agent: string | null;
}

/** What a run of doctor is about: a Worktree, by its path, or `null` for the machine and the Daemon. */
export type Scope = string | null;

/** What the window knows of the checks of one scope. */
export type DoctorState =
  | { kind: "idle" }
  /** A run is on its way, `document` is the last one while there is one, so that a refresh does not blank the list. */
  | { kind: "running"; document: DoctorDocument | null }
  | { kind: "report"; document: DoctorDocument; at: number }
  | { kind: "failure"; message: string };

/** The event the menu sends when ⌘R refreshes, since that refresh does not go through the window. */
export const REFRESH_REQUESTED = "refresh-requested";

/**
 * The checks that need the Daemon, which a run without one skips. The Daemon not running screen leaves them out,
 * and `docs/doctor.md` lists them as the checks it asks the Daemon for.
 */
const DAEMON_CHECKS: readonly string[] = ["leased_ports", "orphans", "portless_aliases"];

/** The checks that depend on the directory doctor runs in, which the Worktree screen shows. */
const WORKTREE_CHECKS: readonly string[] = ["configuration", "guard", "portless", "worktrunk_hooks"];

/** The fix the `daemon` check names for a Daemon of another release, which is how the window tells that drift. */
const RESTART_FIX = "procly daemon stop";

/** The checks of the machine and the Daemon, which the Doctor screen shows: all but those of a Worktree. */
export function machineChecks(checks: Check[]): Check[] {
  return checks.filter((check) => !WORKTREE_CHECKS.includes(check.name));
}

/** The checks of one Worktree. */
export function worktreeChecks(checks: Check[]): Check[] {
  return checks.filter((check) => WORKTREE_CHECKS.includes(check.name));
}

/** The checks that run without a Daemon. */
export function checksWithoutDaemon(checks: Check[]): Check[] {
  return checks.filter((check) => !DAEMON_CHECKS.includes(check.name));
}

const RANK: Record<CheckStatus, number> = { failed: 0, warning: 1, passed: 2, skipped: 3 };

/** Problems first: failed, then warning, then passed, then skipped, in the order of the document within each. */
export function problemsFirst(checks: Check[]): Check[] {
  return checks
    .map((check, index) => ({ check, index }))
    .sort((a, b) => RANK[a.check.status] - RANK[b.check.status] || a.index - b.index)
    .map(({ check }) => check);
}

/**
 * The check of a Daemon that answers in the protocol of this app but runs another release, or `null`. Doctor says it
 * in its `daemon` check, and offers `procly daemon stop` as the remedy.
 */
export function releaseDrift(document: DoctorDocument): Check | null {
  return (
    document.checks.find(
      (check) => check.name === "daemon" && check.status === "failed" && check.fix === RESTART_FIX,
    ) ?? null
  );
}

/**
 * The last document of doctor for each scope. The window never runs it on a timer: a screen asks when it opens, and
 * Refresh asks again for what is on screen.
 */
export interface DoctorStore {
  subscribe(listener: () => void): () => void;
  get(scope: Scope): DoctorState;
  /** Runs doctor for `scope`. A run already on its way answers for this one too. */
  run(scope: Scope): Promise<void>;
  /** A screen shows `scope`: runs doctor for it now, and again on each Refresh until the returned function is called. */
  open(scope: Scope): () => void;
  /** Refresh: runs doctor again for the machine, which the banner reads, and for every scope on screen. */
  refresh(): Promise<void>;
}

const IDLE: DoctorState = { kind: "idle" };

function documentOf(state: DoctorState): DoctorDocument | null {
  switch (state.kind) {
    case "running":
      return state.document;
    case "report":
      return state.document;
    case "idle":
    case "failure":
      return null;
    default: {
      const unhandled: never = state;
      throw new Error(`unhandled state: ${JSON.stringify(unhandled)}`);
    }
  }
}

export function createDoctorStore(): DoctorStore {
  const states = new Map<Scope, DoctorState>();
  const running = new Map<Scope, Promise<void>>();
  const shown = new Map<Scope, number>();
  const listeners = new Set<() => void>();

  function set(scope: Scope, state: DoctorState): void {
    states.set(scope, state);
    for (const listener of listeners) {
      listener();
    }
  }

  const store: DoctorStore = {
    subscribe(listener) {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
    get: (scope) => states.get(scope) ?? IDLE,
    run(scope) {
      const flying = running.get(scope);
      if (flying !== undefined) {
        return flying;
      }
      set(scope, { kind: "running", document: documentOf(store.get(scope)) });
      const run = invoke<DoctorDocument>("doctor", { cwd: scope })
        .then(
          (document) => {
            set(scope, { kind: "report", document, at: Date.now() });
          },
          (reason: unknown) => {
            set(scope, { kind: "failure", message: failureMessage(reason) });
          },
        )
        .finally(() => {
          running.delete(scope);
        });
      running.set(scope, run);
      return run;
    },
    open(scope) {
      shown.set(scope, (shown.get(scope) ?? 0) + 1);
      void store.run(scope);
      return () => {
        const left = (shown.get(scope) ?? 1) - 1;
        if (left <= 0) {
          shown.delete(scope);
        } else {
          shown.set(scope, left);
        }
      };
    },
    async refresh() {
      const scopes = new Set<Scope>([null, ...shown.keys()]);
      await Promise.all([...scopes].map((scope) => store.run(scope)));
    },
  };
  return store;
}

/** The last document of doctor for `scope`, read through `useSyncExternalStore`. */
export function useDoctor(store: DoctorStore, scope: Scope): DoctorState {
  return useSyncExternalStore(store.subscribe, () => store.get(scope));
}

/** Runs doctor for `scope` while the calling screen shows it. */
export function useDoctorOpen(store: DoctorStore, scope: Scope): void {
  useEffect(() => store.open(scope), [store, scope]);
}

/** Calls `handler` whenever the Rust side emits `event`. */
export function useTauriEvent(event: string, handler: () => void): void {
  const latest = useRef(handler);
  useEffect(() => {
    latest.current = handler;
  });
  useEffect(() => {
    const stop = new AbortController();
    void listen(event, () => {
      latest.current();
    }).then((off) => {
      // A listener registered after the screen left is released at once.
      if (stop.signal.aborted) {
        off();
      } else {
        stop.signal.addEventListener("abort", off, { once: true });
      }
    });
    return () => {
      stop.abort();
    };
  }, [event]);
}
