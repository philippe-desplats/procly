// @vitest-environment node
import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

const css = readFileSync(new URL("./styles.css", import.meta.url), "utf8");

/** The declarations of the first rule whose selector, or one of the selectors of its list, is exactly `selector`. */
function declarations(selector: string): string {
  for (const [, selectors = "", body = ""] of css.matchAll(/([^{}]+)\{([^}]*)\}/g)) {
    // A comment sits in front of the first selector of a rule.
    const list = selectors.replace(/\/\*[\s\S]*?\*\//g, "").split(",");
    if (list.some((one) => one.trim() === selector)) {
      return body;
    }
  }
  return "";
}

/** The value of `property` among `body`, `undefined` when it is not declared. */
function value(body: string, property: string): string | undefined {
  return new RegExp(`(?:^|[\\s;])${property}:\\s*([^;]+);`).exec(body)?.[1]?.trim();
}

const px = (text: string | undefined): number => {
  const found = /^(\d+(?:\.\d+)?)px$/.exec(text ?? "");
  return found?.[1] === undefined ? Number.NaN : Number(found[1]);
};

/**
 * What the window really draws, measured on the real app at the widths of the verifier. jsdom lays nothing out, so these
 * stand in for the width of the content the stylesheet has to make room for.
 */
const MEASURED = {
  openInEditor: 108.2,
  openInTerminal: 123.9,
  updatedAt: 110,
  refresh: 68.5,
  /** "Main worktree" in the small size, the widest thing that sits beside the name of a folder. */
  legend: 80,
};

/** The widths the center column takes at the 2 windows that bound the layout: the minimum one once #156 narrows it. */
const COLUMN = { widest: 780, narrowest: 260 };

interface Item {
  name: string;
  /** The width it asks for, which is what decides whether it fits on the line. */
  size: number;
  /** The least it can be squeezed to without something leaving the column. */
  least: number;
}

/**
 * Lays the title line of a Worktree out the way a wrapping flex line does: the items go on a line in turn, and an item
 * that does not fit starts the next one. Returns the lines of items, and what overflows the column.
 */
function layOut(column: number): { lines: Item[][]; overflow: string[]; titleRoom: number } {
  const bar = declarations(".title-bar");
  const [, right = "", , left = ""] = (value(bar, "padding") ?? "").split(/\s+/);
  const inner = column - px(left) - px(right);
  const wraps = value(declarations(".worktree-head"), "flex-wrap") === "wrap";
  const gap = px(value(bar, "gap"));
  const groupGap = px(value(declarations(".worktree-open"), "gap"));
  const groupWraps = value(declarations(".worktree-open"), "flex-wrap") === "wrap";
  const title = px(value(declarations(".worktree-title"), "flex")?.split(/\s+/)[2]);
  const nameLeast = px(value(declarations(".worktree-head .title"), "min-width"));
  const group = (name: string, sizes: number[]): Item => ({
    name,
    size: sizes.reduce((sum, one) => sum + one, 0) + groupGap * (sizes.length - 1),
    // A group that wraps can drop to its widest button, one that does not keeps its whole width.
    least: groupWraps
      ? Math.max(...sizes)
      : sizes.reduce((sum, one) => sum + one, 0) + groupGap * (sizes.length - 1),
  });
  const items: Item[] = [
    { name: "title", size: Number.isNaN(title) ? 0 : title, least: Number.isNaN(nameLeast) ? 0 : nameLeast },
    group("open", [MEASURED.openInEditor, MEASURED.openInTerminal]),
    group("controls", [MEASURED.updatedAt, MEASURED.refresh]),
  ];
  const lines: Item[][] = [[]];
  for (const item of items) {
    const line = lines.at(-1) ?? [];
    const taken = line.reduce((sum, one) => sum + one.size + gap, 0);
    if (wraps && line.length > 0 && taken + item.size > inner) {
      lines.push([item]);
    } else {
      line.push(item);
    }
  }
  const overflow = lines
    .filter((line) => line.reduce((sum, one) => sum + one.size + gap, -gap) > inner)
    .flatMap((line) =>
      // The items of a crowded line give up width down to their least, and what cannot is what leaves the column.
      line.reduce((sum, one) => sum + one.least + gap, -gap) > inner ? line.map((one) => one.name) : [],
    );
  const own = lines.find((line) => line.some((one) => one.name === "title")) ?? [];
  const others = own.filter((one) => one.name !== "title").reduce((sum, one) => sum + one.size + gap, 0);
  return { lines, overflow, titleRoom: inner - others };
}

describe("the title line of a Worktree", () => {
  // jsdom loads no stylesheet and lays nothing out, so the layout is read from the CSS text and worked out from it.
  it("wraps, and is as tall as the strip over the sidebar only while it holds a single line", () => {
    const head = declarations(".worktree-head");
    expect(value(head, "flex-wrap")).toBe("wrap");
    expect(value(head, "height")).toBe("auto");
    expect(value(head, "min-height")).toBe("52px");
  });

  it("stays on one line at the widest window, as it was", () => {
    const { lines, overflow } = layOut(COLUMN.widest);

    expect(lines.map((line) => line.map((item) => item.name))).toEqual([["title", "open", "controls"]]);
    expect(overflow).toEqual([]);
  });

  it("puts the buttons and the controls on lines of their own at the narrowest window, none leaving the column", () => {
    const { lines, overflow } = layOut(COLUMN.narrowest);

    expect(lines.map((line) => line.map((item) => item.name))).toEqual([["title"], ["open"], ["controls"]]);
    expect(overflow).toEqual([]);
  });

  it("leaves the title a width of its own that never falls to nothing, at either window", () => {
    for (const column of Object.values(COLUMN)) {
      const { titleRoom } = layOut(column);
      expect(px(value(declarations(".worktree-title"), "flex")?.split(/\s+/)[2])).toBeGreaterThan(0);
      expect(titleRoom).toBeGreaterThanOrEqual(
        px(value(declarations(".worktree-title"), "flex")?.split(/\s+/)[2]),
      );
    }
  });

  it("keeps the name readable and the legend of the main Worktree beside it rather than under a button", () => {
    const basis = px(value(declarations(".worktree-title"), "flex")?.split(/\s+/)[2]);
    const name = declarations(".worktree-head .title");
    const gap = px(value(declarations(".worktree-name"), "gap"));

    expect(px(value(name, "min-width"))).toBeGreaterThanOrEqual(48);
    expect(value(name, "text-overflow")).toBe("ellipsis");
    expect(value(name, "white-space")).toBe("nowrap");
    // The name at its least, the gap and the legend all fit in the width the title is given, so nothing spills over.
    expect(px(value(name, "min-width")) + gap + MEASURED.legend).toBeLessThanOrEqual(basis);
    expect(value(declarations(".worktree-legend"), "flex")).toBe("none");
    expect(value(declarations(".worktree-legend"), "white-space")).toBe("nowrap");
  });

  it("cuts the path with an ellipsis rather than wrapping it", () => {
    const path = declarations(".worktree-path");
    expect(value(declarations(".worktree-title"), "min-width")).toBe("0");
    expect(value(path, "overflow")).toBe("hidden");
    expect(value(path, "text-overflow")).toBe("ellipsis");
    expect(value(path, "white-space")).toBe("nowrap");
  });

  it("lets the groups of buttons wrap inside the column and keeps each button and the time whole", () => {
    for (const group of [".worktree-open", ".worktree-controls"]) {
      expect(value(declarations(group), "flex-wrap")).toBe("wrap");
      expect(value(declarations(group), "min-width")).toBe("0");
    }
    expect(value(declarations(".worktree-head .button"), "flex")).toBe("none");
    expect(value(declarations(".worktree-head .caption"), "white-space")).toBe("nowrap");
  });

  it("gives Release… the room under the bar that sticks 16px down, then the 12px it has everywhere else", () => {
    expect(px(value(declarations(".actions-row.release-row"), "margin-top"))).toBeGreaterThanOrEqual(16 + 12);
  });
});
