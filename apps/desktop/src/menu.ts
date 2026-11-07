import { invoke } from "@tauri-apps/api/core";
import { useEffect, useRef, useState } from "react";

import type { FailureKey } from "./daemon";
import { sameFailure } from "./needs-you";
import type { MenuContext } from "./protocol/MenuContext";
import type { WorktreeStatus } from "./protocol/WorktreeStatus";
import type { Selected } from "./selection";
import { inWorktree } from "./shell";

/**
 * The event Rust sends when an item of the menu bar the window carries out is chosen, with the id of the item as a
 * `MenuChoice`, the type ts-rs exports from the items of Rust.
 */
export const MENU_CHOICE = "menu-choice";

/** What holds while nothing is selected, which is also what Rust starts from. */
const NOTHING: MenuContext = {
  logs_command: null,
  restart: false,
  stop: false,
  mark_seen: false,
  worktree: null,
  slug: null,
  port: null,
  stop_command: null,
  text_selected: false,
  find: false,
};

/** The port the selection listens on, the first one of a Foreign process that holds several. */
export function portOf(selected: Selected): number | null {
  switch (selected.kind) {
    case "service":
      return selected.service.port;
    case "compose":
      return selected.container.port;
    case "foreign":
      return selected.process.ports[0] ?? null;
    case "job":
      return null;
    default: {
      const unhandled: never = selected;
      throw new Error(`unhandled selection: ${JSON.stringify(unhandled)}`);
    }
  }
}

/** The command that stops a Foreign process, which the developer runs: Procly never stops what it did not start. */
export function stopCommandOf(selected: Selected): string | null {
  return selected.kind === "foreign" && selected.process.kind === "process"
    ? `kill ${String(selected.process.pid)}`
    : null;
}

/** The `procly logs` command of the selection, run where the selection runs. A Foreign process has no logs of Procly's. */
export function logsCommand(selected: Selected): string | null {
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
 * `needsYou`, and the Worktree menu acts on the Worktree the inspector offers to open. `⌘C` copies the main value of
 * the selection unless `textSelected` says text is selected in the window, which the standard Copy copies.
 */
export function menuContext(
  selected: Selected | null,
  current: WorktreeStatus | null,
  failure: FailureKey | null,
  needsYou: FailureKey[],
  textSelected = false,
): MenuContext {
  if (selected === null) {
    return {
      ...NOTHING,
      worktree: current?.path ?? null,
      slug: current?.slug ?? null,
      text_selected: textSelected,
    };
  }
  return {
    logs_command: logsCommand(selected),
    restart: selected.kind === "service" && selected.service.restartable,
    stop: (selected.kind === "service" && selected.service.state !== "ended") || selected.kind === "compose",
    mark_seen: failure !== null && needsYou.some((key) => sameFailure(key, failure)),
    worktree: selected.worktree?.path ?? null,
    slug: selected.worktree?.slug ?? null,
    port: portOf(selected),
    stop_command: stopCommandOf(selected),
    text_selected: textSelected,
    // A Foreign process has no output of Procly's to find in.
    find: selected.kind !== "foreign",
  };
}

/** Whether text is selected in the window: in a field, whose selection the document does not hold, or elsewhere. */
function textIsSelected(): boolean {
  const active = document.activeElement;
  if (active instanceof HTMLInputElement || active instanceof HTMLTextAreaElement) {
    try {
      return active.selectionStart !== null && active.selectionStart !== active.selectionEnd;
    } catch {
      // A field of a type that has no text selection, a checkbox for one.
      return false;
    }
  }
  const selection = document.getSelection();
  return selection !== null && !selection.isCollapsed && selection.toString() !== "";
}

/** Follows whether text is selected in the window, so that `⌘C` copies it rather than the main value. */
export function useTextSelected(): boolean {
  const [selected, setSelected] = useState(false);
  useEffect(() => {
    const follow = (): void => {
      setSelected(textIsSelected());
    };
    document.addEventListener("selectionchange", follow);
    return () => {
      document.removeEventListener("selectionchange", follow);
    };
  }, []);
  return selected;
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
