import { useState } from "react";

import { CopyBox } from "./CopyBox";
import { fixCommands, problemsFirst, type DoctorState } from "./doctor";
import { GlyphMark } from "./Glyph";
import type { Check } from "./protocol/Check";
import type { CheckStatus } from "./protocol/CheckStatus";
import type { StateWord } from "./state";

function statusWord(status: CheckStatus): StateWord {
  switch (status) {
    case "failed":
      return { word: "failed", tone: "fail", glyph: "failure" };
    case "warning":
      return { word: "warning", tone: "warn", glyph: "warning" };
    case "passed":
      return { word: "passed", tone: "ok", glyph: "passed" };
    case "skipped":
      return { word: "skipped", tone: "muted", glyph: "skipped" };
    default: {
      // A status a newer procly may add counts as no problem, as the JSON reference says.
      const unknown: never = status;
      return { word: String(unknown), tone: "muted", glyph: "skipped" };
    }
  }
}

/** A button on a check that takes the developer to where its problem is. */
export interface ShowAction {
  label: string;
  onShow: () => void;
}

/** The action a check offers, if any. */
export type ShowActions = (check: Check) => ShowAction | null;

/** The commands of a fix, one per line, each behind a Copy button that copies exactly what it shows. */
function Fix({ fix, worktree }: { fix: string; worktree: string | null }) {
  const commands = fixCommands(fix, worktree);
  return (
    <div className="fix">
      {commands.map((command) => (
        <CopyBox key={command} text={command} />
      ))}
    </div>
  );
}

/**
 * The glyph of a status. It is decorative, so the word of the status is kept as text a screen reader reads and the eye
 * does not see.
 */
function StatusGlyph({ status }: { status: CheckStatus }) {
  const { tone, glyph } = statusWord(status);
  return (
    <span className={`check-glyph tone-${tone}`}>
      <GlyphMark glyph={glyph} />
    </span>
  );
}

/** One check: the message as the main line, the name of the check as a caption, then the fix and where to look. */
function CheckItem({
  check,
  worktree,
  show,
}: {
  check: Check;
  worktree: string | null;
  show: ShowActions | undefined;
}) {
  const action = show?.(check) ?? null;
  return (
    <li className="item check" data-check={check.name} data-status={check.status}>
      <StatusGlyph status={check.status} />
      <div className="check-text">
        <span className="visually-hidden">{statusWord(check.status).word}</span>
        <p className="check-message">{check.message}</p>
        <code className="check-name">{check.name}</code>
      </div>
      {check.fix === null && action === null ? null : (
        <div className="check-fixes">
          {check.fix === null ? null : <Fix fix={check.fix} worktree={worktree} />}
          {action === null ? null : (
            <button type="button" className="button" onClick={action.onShow}>
              {action.label}
            </button>
          )}
        </div>
      )}
    </li>
  );
}

/** Whether a check is something to act on: a status a newer procly may add counts as no problem. */
function isProblem(check: Check): boolean {
  return check.status === "failed" || check.status === "warning";
}

/** "4 checks passed, 1 skipped": what the fold holds, with the skipped checks named only when there are some. */
function foldedText(folded: Check[]): string {
  const skipped = folded.filter((check) => check.status === "skipped").length;
  const passed = folded.length - skipped;
  const checks = (count: number): string => (count === 1 ? "1 check" : `${String(count)} checks`);
  if (passed === 0) {
    return `${checks(skipped)} skipped`;
  }
  return skipped === 0 ? `${checks(passed)} passed` : `${checks(passed)} passed, ${String(skipped)} skipped`;
}

/** The line the passed and skipped checks fold into, with the button that unfolds them. */
function FoldRow({ folded, open, onToggle }: { folded: Check[]; open: boolean; onToggle: () => void }) {
  const passed = folded.some((check) => check.status === "passed");
  return (
    <li className="item check-fold">
      <StatusGlyph status={passed ? "passed" : "skipped"} />
      <div className="check-fold-line">
        <span className="check-fold-text">{foldedText(folded)}</span>
        <button type="button" className="fold-toggle" aria-expanded={open} onClick={onToggle}>
          {open ? "Hide" : "Show"}
        </button>
      </div>
    </li>
  );
}

/**
 * The checks, the failed and the warnings first, each with its message and the fix doctor names for it. With `fold`,
 * what passed or was skipped sits behind one line and a Show button, so that the problems are all that is in view.
 */
export function CheckList({
  checks,
  worktree,
  fold = false,
  show,
}: {
  checks: Check[];
  worktree: string | null;
  fold?: boolean;
  show?: ShowActions | undefined;
}) {
  const [open, setOpen] = useState(false);
  const ordered = problemsFirst(checks);
  const upfront = fold ? ordered.filter(isProblem) : ordered;
  const folded = fold ? ordered.filter((check) => !isProblem(check)) : [];
  return (
    <ul className="list checks">
      {upfront.map((check) => (
        <CheckItem key={check.name} check={check} worktree={worktree} show={show} />
      ))}
      {folded.length === 0 ? null : (
        <FoldRow
          folded={folded}
          open={open}
          onToggle={() => {
            setOpen(!open);
          }}
        />
      )}
      {open
        ? folded.map((check) => <CheckItem key={check.name} check={check} worktree={worktree} show={show} />)
        : null}
    </ul>
  );
}

/**
 * What a scope shows: its checks once doctor answered, why it could not run, or that it is running. `pick` chooses the
 * checks of the document that belong on the screen, and `worktree` is the directory the fixes run in, `null` for the
 * machine. `fold` folds the checks that passed, and `show` gives a check the button that takes to its problem.
 */
export function Checks({
  state,
  pick,
  worktree = null,
  fold = false,
  show,
}: {
  state: DoctorState;
  pick: (checks: Check[]) => Check[];
  worktree?: string | null;
  fold?: boolean;
  show?: ShowActions | undefined;
}) {
  switch (state.kind) {
    case "idle":
      return null;
    case "running":
      return state.document === null ? (
        <p className="caption">Checking…</p>
      ) : (
        <CheckList checks={pick(state.document.checks)} worktree={worktree} fold={fold} show={show} />
      );
    case "report":
      return <CheckList checks={pick(state.document.checks)} worktree={worktree} fold={fold} show={show} />;
    case "failure":
      return (
        <p role="alert" className="error">
          {state.message}
        </p>
      );
    default: {
      const unhandled: never = state;
      throw new Error(`unhandled state: ${JSON.stringify(unhandled)}`);
    }
  }
}
