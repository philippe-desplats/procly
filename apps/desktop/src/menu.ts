import { invoke } from "@tauri-apps/api/core";
import { useEffect, useRef } from "react";

import type { FailureKey } from "./daemon";
import { sameFailure } from "./needs-you";
import type { MenuContext } from "./protocol/MenuContext";
import type { WorktreeStatus } from "./protocol/WorktreeStatus";
import type { Selected } from "./selection";
import { inWorktree } from "./shell";

/** The event Rust sends when an item of the menu bar the window carries out is chosen. */
export const MENU_CHOICE = "menu-choice";

/** The items of the menu bar the window carries out, by the id Rust gives them. */
export type MenuChoice =
  | "overview"
  | "doctor"
  | "orphans"
  | "restart"
  | "stop"
  | "mark-seen"
  | "open-in-editor"
  | "open-in-terminal"
  | "release";

/** What holds while nothing is selected, which is also what Rust starts from. */
const NOTHING: MenuContext = {
  logs_command: null,
  restart: false,
  stop: false,
  mark_seen: false,
  worktree: null,
};

/** The `procly logs` command of the selection, run where the selection runs. A Foreign process has no logs of Procly's. */
function logsCommand(selected: Selected): string | null {
  switch (selected.kind) {
    case "service":
      return inWorktree(selected.worktree.path, ["logs", selected.service.service]);
    case "compose":
      return inWorktree(selected.worktree.path, ["logs", selected.container.service]);
    case "job":
      return inWorktree(selected.job.cwd, ["logs", selected.job.id]);
    case "foreign":
      return null;
    default: {
      const unhandled: never = selected;
      throw new Error(`unhandled selection: ${JSON.stringify(unhandled)}`);
    }
  }
}

/**
 * What the menu bar may do with what the inspector shows: `selected`, or the Worktree `current` when nothing is picked.
 * Restart and Stop follow the buttons of the inspector, Mark as Seen needs a failure of "Needs you", whose keys are
 * `needsYou`, and the Worktree menu acts on the Worktree the inspector offers to open.
 */
export function menuContext(
  selected: Selected | null,
  current: WorktreeStatus | null,
  failure: FailureKey | null,
  needsYou: FailureKey[],
): MenuContext {
  if (selected === null) {
    return { ...NOTHING, worktree: current?.path ?? null };
  }
  return {
    logs_command: logsCommand(selected),
    restart: selected.kind === "service" && selected.service.restartable,
    stop: (selected.kind === "service" && selected.service.state !== "ended") || selected.kind === "compose",
    mark_seen: failure !== null && needsYou.some((key) => sameFailure(key, failure)),
    worktree: selected.worktree?.path ?? null,
  };
}

/**
 * Tells Rust what the menu bar may do each time it changes. Nothing is sent while it is what Rust starts from, and a
 * failure to send leaves the items as they were.
 */
export function useMenuContext(context: MenuContext): void {
  const sent = useRef(JSON.stringify(NOTHING));
  const next = JSON.stringify(context);
  useEffect(() => {
    if (next !== sent.current) {
      sent.current = next;
      invoke("set_menu_context", { context }).catch(() => undefined);
    }
  });
}
