import type { FailureKey, NeedsYou } from "./daemon";

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
