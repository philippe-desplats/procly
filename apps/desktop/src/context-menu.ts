import { invoke } from "@tauri-apps/api/core";
import { useCallback, useRef } from "react";

import { useTauriEvent } from "./doctor";
import { logsCommand, portOf, stopCommandOf } from "./menu";
import type { ContextEntry } from "./protocol/ContextEntry";
import type { MenuChoice } from "./protocol/MenuChoice";
import type { WorktreeStatus } from "./protocol/WorktreeStatus";
import type { Selected } from "./selection";
import { inWorktree } from "./shell";

/** The event Rust sends with the index of the entry chosen in the context menu the window showed last. */
export const CONTEXT_CHOICE = "context-choice";

/**
 * One line of a context menu as the window builds it. Rust draws it and copies the text of a copy line itself, the
 * window runs an action line once Rust tells it which was chosen.
 */
export type MenuLine =
  | { kind: "copy"; title: string; text: string }
  | { kind: "act"; title: string; run: () => void; accelerator?: MenuChoice }
  | { kind: "separator" };

const SEPARATOR: MenuLine = { kind: "separator" };

function copy(title: string, text: string): MenuLine {
  return { kind: "copy", title, text };
}

/** The lines as Rust receives them, without what the window runs. */
export function entriesOf(lines: MenuLine[]): ContextEntry[] {
  return lines.map((line) => {
    if (line.kind !== "act") {
      return line;
    }
    return line.accelerator === undefined
      ? { kind: "act", title: line.title }
      : { kind: "act", title: line.title, accelerator: line.accelerator };
  });
}

/** Whether a failure is Seen, and how to mark it either way. */
export interface Marks {
  seen: boolean;
  markSeen: () => void;
  markUnseen: () => void;
}

/**
 * The context menu of a row, as the mockup draws it: the command that stops a Foreign process, the port, the command
 * line of a Job, the logs command, the command that restarts a Service, then, after a separator, Stop when `stop` is
 * given, which a Service or an Isolated Compose service that runs has, and Mark as Seen or Mark as Unseen when the row
 * is a failure, which `marks` says. Stop shows `⌘.`, the key of the Stop of the Actions menu: the line names that item
 * and Rust reads the key from it, so the 2 cannot drift. The key only shows here, the menu bar acts on it.
 */
export function rowMenu(
  selected: Selected,
  marks: Marks | null,
  stop: (() => void) | null = null,
): MenuLine[] {
  const lines: MenuLine[] = [];
  const kill = stopCommandOf(selected);
  if (kill !== null) {
    lines.push(copy(`Copy ${kill}`, kill));
  }
  const port = portOf(selected);
  if (port !== null) {
    lines.push(copy(`Copy Port ${String(port)}`, String(port)));
  } else if (selected.kind === "job") {
    lines.push(copy("Copy Command Line", selected.job.command));
  }
  const logs = logsCommand(selected);
  if (logs !== null) {
    lines.push(copy("Copy Logs Command", logs));
  }
  if (selected.kind === "service") {
    lines.push(
      copy("Copy Restart Command", inWorktree(selected.worktree.path, ["restart", selected.service.service])),
    );
  }
  const after: MenuLine[] = [];
  if (stop !== null) {
    after.push({ kind: "act", title: "Stop", run: stop, accelerator: "stop" });
  }
  if (marks !== null) {
    after.push(
      marks.seen
        ? { kind: "act", title: "Mark as Unseen", run: marks.markUnseen }
        : { kind: "act", title: "Mark as Seen", run: marks.markSeen },
    );
  }
  return after.length === 0 ? lines : [...lines, SEPARATOR, ...after];
}

/** What the context menu of a Worktree does with it. */
export interface WorktreeActs {
  openInEditor: () => void;
  openInTerminal: () => void;
  reveal: () => void;
  /** Opens the screen of the Worktree and runs the doctor checks of its directory. */
  check: () => void;
  release: () => void;
}

/** The context menu of a Worktree in the sidebar: the items of the Worktree menu of the menu bar, and Check This Worktree. */
export function worktreeMenu(worktree: WorktreeStatus, acts: WorktreeActs): MenuLine[] {
  return [
    { kind: "act", title: "Open in Editor", run: acts.openInEditor },
    { kind: "act", title: "Open in Terminal", run: acts.openInTerminal },
    { kind: "act", title: "Reveal in Finder", run: acts.reveal },
    SEPARATOR,
    copy("Copy Path", worktree.path),
    copy("Copy Slug", worktree.slug),
    SEPARATOR,
    { kind: "act", title: "Check This Worktree", run: acts.check },
    SEPARATOR,
    { kind: "act", title: "Release Worktree…", run: acts.release },
  ];
}

/**
 * Shows a context menu through Rust, natively at the pointer, and runs the action line Rust says was chosen. A menu
 * that cannot be shown leaves the same actions in the menu bar and the inspector.
 */
export function useContextMenu(): (lines: MenuLine[]) => void {
  const shown = useRef<MenuLine[]>([]);
  useTauriEvent<number>(CONTEXT_CHOICE, (index) => {
    const line = shown.current[index];
    if (line?.kind === "act") {
      line.run();
    }
  });
  return useCallback((lines: MenuLine[]) => {
    shown.current = lines;
    invoke("show_context_menu", { entries: entriesOf(lines) }).catch(() => undefined);
  }, []);
}
