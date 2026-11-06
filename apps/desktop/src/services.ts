import { invoke } from "@tauri-apps/api/core";

import type { Response } from "./protocol/Response";
import { serviceEndWord } from "./state";

const SAFE_PATH = /^[A-Za-z0-9_@%+=:,./-]+$/;

/** A path as a shell reads it as one word: left as it is when it needs no quoting, in single quotes otherwise. */
function shellWord(path: string): string {
  return SAFE_PATH.test(path) ? path : `'${path.replaceAll("'", "'\\''")}'`;
}

/**
 * A `procly` command to run from the Worktree at `path`, written so that a paste runs it there whatever the shell is in:
 * `(cd <path> && procly <command>)`. The parentheses keep the `cd` from leaving the developer's shell elsewhere.
 */
export function commandIn(path: string, command: string): string {
  return `(cd ${shellWord(path)} && procly ${command})`;
}

/** Why an answer is not the one the action expects, or null when it is. */
function refusalOf(response: Response, expected: "down" | "up"): string | null {
  if (response.response === expected) {
    return null;
  }
  switch (response.response) {
    case "error":
      return response.message;
    case "unhealthy":
      return `${response.service} did not become healthy: ${serviceEndWord(response.end).word}`;
    case "conflict":
      return `${response.service} cannot start, a process outside Procly holds what it needs`;
    default:
      return `unexpected answer from the Daemon: ${response.response}`;
  }
}

async function ask(command: "stop_service" | "restart_service", service: string, cwd: string): Promise<void> {
  const response = await invoke<Response>(command, { service, cwd });
  const refusal = refusalOf(response, command === "stop_service" ? "down" : "up");
  if (refusal !== null) {
    throw new Error(refusal);
  }
}

/**
 * Stops a Service or an Isolated Compose service of the Worktree at `cwd`. There is nothing to confirm, a Service
 * starts again with a Restart. Rejects with why the Daemon refused.
 */
export const stopService = (service: string, cwd: string): Promise<void> => ask("stop_service", service, cwd);

/**
 * Restarts a Service with the environment the Daemon holds from its last `up`, so the window never gives one of its own.
 * Rejects with why it did not come back healthy.
 */
export const restartService = (service: string, cwd: string): Promise<void> =>
  ask("restart_service", service, cwd);
