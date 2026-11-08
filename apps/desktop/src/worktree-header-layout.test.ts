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
 * What the window really draws, measured on the real app by the verifier. jsdom lays nothing out, so these stand in
 * for the width of the content the stylesheet has to make room for.
 */
const MEASURED = {
  openInEditor: 108.2,
  openInTerminal: 123.9,
  updatedAt: 110,
  refresh: 68.5,
  /** "Main worktree" in the small size, the widest thing that sits beside the name of a folder. */
  legend: 80,
};

/**
 * The name of a folder in the title size, a little wider than `acme-web`, which the 164 px title column of the
 * previous layout cut at 1280 px together with its legend.
 */
const NAME = 90;

/** The widths the center column takes at the 2 windows that bound the layout: 1280 px, and the minimum of 960 px. */
const COLUMN = { at1280: 660, narrowest: 380 };

interface Item {
  name: string;
  /** The width it asks for, which is what decides whether it fits on the line. */
  size: number;
  /** The least it can be squeezed to without something leaving the column or being cut. */
  least: number;
}

/**
 * Lays the title line of a Worktree out the way a wrapping flex line does: the items go on a line in turn, and an item
 * that does not fit starts the next one. Returns the lines of items, what overflows the column, and the width the
 * title gets on its own line.
 */
function layOut(column: number): { lines: Item[][]; overflow: string[]; titleWidth: number } {
  const bar = declarations(".title-bar");
  const [, right = "", , left = ""] = (value(bar, "padding") ?? "").split(/\s+/);
  const inner = column - px(left) - px(right);
  const wraps = value(bar, "flex-wrap") === "wrap";
  const gaps = (value(bar, "gap") ?? "").split(/\s+/);
  const gap = px(gaps[1] ?? gaps[0]);
  const groupGap = px(value(declarations(".title-actions"), "gap"));
  const block = (value(declarations(".title-block"), "flex") ?? "").split(/\s+/);
  // A basis of auto is the width of its words: the name, the gap of its line and the legend. The path under it adds
  // nothing, it is given no width of its own.
  const words = NAME + px(value(declarations(".title-line"), "gap")) + MEASURED.legend;
  const shrinks = block[1] !== "0";
  const group = (name: string, sizes: number[]): Item => {
    const size = sizes.reduce((sum, one) => sum + one, 0) + groupGap * (sizes.length - 1);
    return { name, size, least: Math.max(...sizes) };
  };
  const items: Item[] = [
    { name: "title", size: block[2] === "auto" ? words : px(block[2]), least: shrinks ? 0 : words },
    group("actions", [MEASURED.openInEditor, MEASURED.openInTerminal]),
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
  // The title grows into what its line leaves, and never shrinks below its words.
  return { lines, overflow, titleWidth: Math.max(inner - others, shrinks ? 0 : words) };
}

describe("the title line of a screen", () => {
  // jsdom loads no stylesheet and lays nothing out, so the layout is read from the CSS text and worked out from it.
  it("wraps, and is as tall as the strip over the sidebar while it holds a single line", () => {
    const bar = declarations(".title-bar");
    expect(value(bar, "flex-wrap")).toBe("wrap");
    expect(value(bar, "height")).toBe("auto");
    expect(value(bar, "min-height")).toBe("52px");
  });

  it("shows the name of a Worktree whole at 1280 px, on one line with its buttons and controls", () => {
    const { lines, overflow, titleWidth } = layOut(COLUMN.at1280);

    expect(lines.map((line) => line.map((item) => item.name))).toEqual([["title", "actions", "controls"]]);
    expect(overflow).toEqual([]);
    expect(titleWidth).toBeGreaterThanOrEqual(NAME + MEASURED.legend);
  });

  it("puts the buttons and the controls under the title at the narrowest window, none leaving the column", () => {
    const { lines, overflow, titleWidth } = layOut(COLUMN.narrowest);

    expect(lines.map((line) => line.map((item) => item.name))).toEqual([
      ["title"],
      ["actions"],
      ["controls"],
    ]);
    expect(overflow).toEqual([]);
    expect(titleWidth).toBeGreaterThanOrEqual(NAME + MEASURED.legend);
  });

  it("never shrinks the title below its words, and only cuts it at the width of the column", () => {
    const block = declarations(".title-block");
    expect(value(block, "flex")).toBe("1 0 auto");
    expect(value(block, "max-width")).toBe("100%");
    expect(value(block, "min-width")).toBe("0");
    const title = declarations(".title-bar .title");
    expect(value(title, "text-overflow")).toBe("ellipsis");
    expect(value(title, "white-space")).toBe("nowrap");
    expect(value(declarations(".title-legend"), "flex")).toBe("none");
    expect(value(declarations(".title-legend"), "white-space")).toBe("nowrap");
  });

  it("gives the path under the title no width of its own and cuts it with an ellipsis", () => {
    const path = declarations(".title-under");
    expect(value(path, "width")).toBe("0");
    expect(value(path, "min-width")).toBe("100%");
    expect(value(path, "overflow")).toBe("hidden");
    expect(value(path, "text-overflow")).toBe("ellipsis");
    expect(value(path, "white-space")).toBe("nowrap");
  });

  it("lets the groups of buttons wrap inside the column and keeps each button and the time whole", () => {
    for (const group of [".title-actions", ".title-controls"]) {
      expect(value(declarations(group), "flex-wrap")).toBe("wrap");
      expect(value(declarations(group), "min-width")).toBe("0");
    }
    expect(value(declarations(".title-bar .button"), "flex")).toBe("none");
    expect(value(declarations(".title-bar .caption"), "white-space")).toBe("nowrap");
  });

  it("gives Release… the room under the bar that sticks 16px down, then the 12px it has everywhere else", () => {
    expect(px(value(declarations(".actions-row.release-row"), "margin-top"))).toBeGreaterThanOrEqual(16 + 12);
  });
});
