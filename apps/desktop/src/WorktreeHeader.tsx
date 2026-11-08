import type { ReactNode } from "react";

import { basename, worktreeTooltip } from "./labels";
import type { WorktreeStatus } from "./protocol/WorktreeStatus";
import { RemovedMark } from "./Rows";
import { openWorktree } from "./settings";
import { TitleBar } from "./TitleBar";
import { useOpening } from "./useOpening";

/**
 * The title line of a Worktree: its folder as the name and its path under it, with the 3 names it goes by in the
 * tooltip, then the two actions that open it, and at the end what every title line says about the answer it shows. The
 * label by branch stays in the sidebar, where the spec states it, so the title is what tells the folders apart.
 */
export function WorktreeHeader({
  worktree,
  controls,
  onNeedEditor,
}: {
  worktree: WorktreeStatus;
  /** The end of the title line: when the answer was read, and Refresh. */
  controls: ReactNode;
  onNeedEditor: () => void;
}) {
  const { error, run } = useOpening(onNeedEditor);
  return (
    <>
      <TitleBar
        title={basename(worktree.path)}
        tooltip={worktreeTooltip(worktree)}
        beside={
          <>
            <RemovedMark worktree={worktree} />
            {worktree.main ? <span className="title-legend">Main worktree</span> : null}
          </>
        }
        under={worktree.path}
        actions={
          <>
            <button
              type="button"
              className="button"
              onClick={() => {
                run(openWorktree(worktree.path, "editor"));
              }}
            >
              Open in Editor
            </button>
            <button
              type="button"
              className="button"
              onClick={() => {
                run(openWorktree(worktree.path, "terminal"));
              }}
            >
              Open in Terminal
            </button>
          </>
        }
      >
        {controls}
      </TitleBar>
      {error === null ? null : (
        <p role="alert" className="error">
          {error}
        </p>
      )}
    </>
  );
}
