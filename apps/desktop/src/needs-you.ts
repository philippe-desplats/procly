import { createContext, useContext } from "react";

import type { FailureKey, NeedsYou } from "./daemon";
import { foreignWord, jobEndWord, serviceEndWord, type Glyph, type Tone } from "./state";

/** Whether two keys name the same failure: a Service that started again is another one. */
export function sameFailure(a: FailureKey, b: FailureKey): boolean {
  switch (a.kind) {
    case "service":
      return (
        b.kind === "service" &&
        a.worktree === b.worktree &&
        a.service === b.service &&
        a.started_at === b.started_at
      );
    case "job":
      return b.kind === "job" && a.id === b.id;
    default: {
      const unhandled: never = a;
      throw new Error(`unhandled key: ${JSON.stringify(unhandled)}`);
    }
  }
}

/** Whether the developer marked the failure as Seen. Rust holds the set, the window only reads it. */
export function isSeen(seen: FailureKey[], key: FailureKey): boolean {
  return seen.some((marked) => sameFailure(marked, key));
}

/** The key to send back to mark a line as Seen, nothing for a Foreign process, which is not an ended failure. */
export function failureKey(line: NeedsYou): FailureKey | null {
  switch (line.kind) {
    case "service":
      return { kind: "service", worktree: line.worktree, service: line.service, started_at: line.started_at };
    case "job":
      return { kind: "job", id: line.id };
    case "foreign":
      return null;
    default: {
      const unhandled: never = line;
      throw new Error(`unhandled line: ${JSON.stringify(unhandled)}`);
    }
  }
}

/**
 * The failures of the answer on screen, as Rust decided them: every line of "Needs you" that is a failure, and every
 * failure marked as Seen. An ended Service or Job that is in neither ended in a routine way.
 */
export function failuresOf(needsYou: NeedsYou[], seen: FailureKey[]): FailureKey[] {
  return [...needsYou.flatMap((line) => failureKey(line) ?? []), ...seen];
}

/** The failures of the answer on screen, which the window provides to the rows and the inspector that draw an end. */
export const FailuresContext = createContext<FailureKey[]>([]);

/** Whether the item `key` names ended in a failure, as Rust said it. */
export function useFailed(key: FailureKey | null): boolean {
  const failures = useContext(FailuresContext);
  return key !== null && failures.some((failure) => sameFailure(failure, key));
}

/** Why a Worktree needs the developer, in the words and the glyph of the item that says it. */
export interface Reason {
  text: string;
  tone: Tone;
  glyph: Glyph;
}

/** What one line of "Needs you" says about itself: the item, then how it ended. */
function reasonOfLine(line: NeedsYou): Reason {
  switch (line.kind) {
    case "service": {
      // A line of "Needs you" that is not a Foreign process is a failure: Rust put it there.
      const { word, tone, glyph } = serviceEndWord(line.end, true);
      return { text: `${line.service} ${word}`, tone, glyph };
    }
    case "job": {
      const { word, tone, glyph } = jobEndWord(line.end, true);
      return { text: `${line.command} ${word}`, tone, glyph };
    }
    case "foreign": {
      // The line reads alone under a label, with no process name before it, so it names the process instead of its state.
      const { tone, glyph } = foreignWord();
      return { text: "a process outside Procly", tone, glyph };
    }
    default: {
      const unhandled: never = line;
      throw new Error(`unhandled line: ${JSON.stringify(unhandled)}`);
    }
  }
}

/**
 * Why the Worktree at `path` needs the developer: the first item of "Needs you" that sits in it, nothing when none
 * does. A Job outside every Repository sits in no Worktree. A failure marked as Seen has left the list, so it says
 * nothing here either.
 */
export function reasonOf(needsYou: NeedsYou[], path: string): Reason | null {
  const first = needsYou.find((line) => line.worktree === path);
  return first === undefined ? null : reasonOfLine(first);
}
