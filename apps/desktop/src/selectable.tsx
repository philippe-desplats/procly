import { createContext, useContext, type ReactNode } from "react";

import { sameSelection, type Selection } from "./selection";

/** What is selected in the center, and how a row selects itself. The center draws rows, the window keeps the selection. */
export interface SelectionControl {
  selection: Selection | null;
  select: (selection: Selection) => void;
  /** Shows the context menu of a row, which a right-click on it asks for once the row is selected. */
  showMenu: (selection: Selection) => void;
}

export const SelectionContext = createContext<SelectionControl>({
  selection: null,
  select: () => undefined,
  showMenu: () => undefined,
});

/** A row of the center that the developer can pick, to have the inspector explain it and follow its output. */
export function SelectableRow({
  target,
  seen,
  wide = false,
  children,
}: {
  target: Selection;
  seen: boolean;
  /** Whether the row has a column for where the item runs, between what it is and its time and ports. */
  wide?: boolean;
  children: ReactNode;
}) {
  const { selection, select, showMenu } = useContext(SelectionContext);
  const picked = sameSelection(selection, target);
  return (
    <li className="item" data-seen={seen ? "true" : undefined} data-selected={picked ? "true" : undefined}>
      <button
        type="button"
        className={wide ? "item-select item-select-wide" : "item-select"}
        aria-pressed={picked}
        onClick={() => {
          select(target);
        }}
        onContextMenu={(event) => {
          // The native menu of the row replaces the one of the webview, as in a list of the Finder.
          event.preventDefault();
          select(target);
          showMenu(target);
        }}
      >
        {children}
      </button>
    </li>
  );
}
