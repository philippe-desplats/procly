import type { ReactNode } from "react";

import { PortPills } from "./PortPill";
import { SelectableRow } from "./selectable";
import type { Selection } from "./selection";
import type { StateWord } from "./state";
import { StateGlyph } from "./StateMark";

/**
 * A row of the center, the same for a Service, a Job, a Foreign process and a line of Needs you: the glyph of the state
 * first, then the name followed by the word of the state, then where the item runs, then its time and its ports.
 */
export function ItemRow({
  target,
  seen,
  state,
  name,
  mono = false,
  detail,
  where,
  whereTitle,
  moment,
  ports = [],
  children,
}: {
  target: Selection;
  seen: boolean;
  state: StateWord;
  name: string;
  /** Whether the name is a command or a path, which is set in the monospace face. */
  mono?: boolean;
  /** What the name leaves to say about the item, quietly: a pid, the Compose project. */
  detail?: string;
  /** Where the item runs, which gives the row a column for it. */
  where?: ReactNode;
  /** The tooltip of `where`, for a Worktree that has more names than the one shown. */
  whereTitle?: string;
  /** When the item started, already worded. */
  moment?: string;
  ports?: number[];
  /** What else the row says at its end, before the time: a Job id, the mark of a Seen failure. */
  children?: ReactNode;
}) {
  return (
    <SelectableRow target={target} seen={seen} wide={where !== undefined}>
      <StateGlyph tone={state.tone} glyph={state.glyph} />
      <span className="item-main">
        <span className={mono ? "item-name mono" : "item-name"}>{name}</span>{" "}
        {detail === undefined ? null : (
          <>
            <span className="caption">{detail}</span>{" "}
          </>
        )}
        <span className={`word tone-${state.tone}`}>{state.word}</span>
      </span>
      {where === undefined ? null : (
        <span className="item-where" title={whereTitle}>
          {where}
        </span>
      )}
      <span className="item-end">
        {children}
        {moment === undefined ? null : <span className="caption">{moment}</span>}
        <span className="item-ports">
          <PortPills ports={ports} />
        </span>
      </span>
    </SelectableRow>
  );
}
