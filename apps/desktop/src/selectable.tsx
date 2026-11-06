import { createContext, useContext, type ReactNode } from "react";

import { sameSelection, type Selection } from "./selection";

/** What is selected in the center, and how a row selects itself. The center draws rows, the window keeps the selection. */
export interface SelectionControl {
  selection: Selection | null;
  select: (selection: Selection) => void;
}

export const SelectionContext = createContext<SelectionControl>({
  selection: null,
  select: () => undefined,
});

/** A row of the center that the developer can pick, to have the inspector explain it and follow its output. */
export function SelectableRow({
  target,
  seen,
  children,
}: {
  target: Selection;
  seen: boolean;
  children: ReactNode;
}) {
  const { selection, select } = useContext(SelectionContext);
  const picked = sameSelection(selection, target);
  return (
    <li className="item" data-seen={seen ? "true" : undefined} data-selected={picked ? "true" : undefined}>
      <button
        type="button"
        className="item-select"
        aria-pressed={picked}
        onClick={() => {
          select(target);
        }}
      >
        {children}
      </button>
    </li>
  );
}
