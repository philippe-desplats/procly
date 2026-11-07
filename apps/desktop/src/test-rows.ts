/** What a row of the center says, read from its parts rather than from its text. */
export interface RowParts {
  /** The shape of the glyph that opens the row, `null` when the row does not open with one. */
  glyph: string | null;
  name: string | undefined;
  word: string | undefined;
  where: string | undefined;
  ports: string[];
}

/** The parts of the row `item`, an `li` of a list of the center. */
export function partsOf(item: HTMLElement): RowParts {
  const select = item.querySelector(".item-select");
  const first = select?.firstElementChild ?? null;
  return {
    glyph: first?.querySelector("svg")?.getAttribute("data-glyph") ?? null,
    name: item.querySelector(".item-name")?.textContent ?? undefined,
    word: item.querySelector(".word")?.textContent ?? undefined,
    where: item.querySelector(".item-where")?.textContent ?? undefined,
    ports: Array.from(item.querySelectorAll(".port")).map((port) => port.textContent ?? ""),
  };
}

/**
 * A matcher for the Daemon line of the sidebar by its whole text. The line holds its words and its time in two elements
 * so that a narrow sidebar cuts the words and never the time, so no single text node carries it.
 */
export function daemonLine(text: string): (content: string, element: Element | null) => boolean {
  return (_content, element) =>
    element?.classList.contains("daemon-line-text") === true && element.textContent === text;
}
