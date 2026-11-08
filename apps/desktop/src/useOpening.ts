import { useState } from "react";

import { failureOf } from "./settings";

/**
 * Runs an action on a Worktree that opens something outside the window, and keeps the failure for the screen to show.
 * A failure that says no editor is set sends the developer to the editor field of the settings instead, since that is
 * the one failure they can fix on the spot.
 */
export function useOpening(onNeedEditor: () => void): {
  /** The message of the last failure that is not a missing editor, `null` after a success. */
  error: string | null;
  run: (action: Promise<void>) => void;
} {
  const [error, setError] = useState<string | null>(null);

  function run(action: Promise<void>): void {
    setError(null);
    action.catch((reason: unknown) => {
      const failure = failureOf(reason);
      if (failure.kind === "no_editor") {
        onNeedEditor();
      } else {
        setError(failure.message);
      }
    });
  }

  return { error, run };
}
