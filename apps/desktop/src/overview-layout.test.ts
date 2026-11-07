// @vitest-environment node
import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

const css = readFileSync(new URL("./styles.css", import.meta.url), "utf8");

/** The declarations of the first rule whose selector is exactly `selector`. */
function declarations(selector: string): string {
  const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const match = new RegExp(`(?:^|\\})\\s*${escaped}\\s*\\{([^}]*)\\}`, "m").exec(css);
  return match?.[1] ?? "";
}

describe("a row of the center", () => {
  // jsdom loads no stylesheet, so the layout of a row is read from the CSS text.
  it("starts with a column for the glyph, then the name, then the time and the ports", () => {
    expect(declarations(".item-select")).toMatch(/display:\s*grid\s*;/);
    expect(declarations(".item-select")).toMatch(/grid-template-columns:\s*14px minmax\(0, 1fr\) auto\s*;/);
  });

  it("has a column for where it runs when it says so, between the name and the ports, narrower than the name", () => {
    expect(declarations(".item-select-wide")).toMatch(
      /grid-template-columns:\s*14px minmax\(0, 2fr\) minmax\(0, 1fr\) auto\s*;/,
    );
  });

  it("keeps the name and its word on one line, never wrapping them", () => {
    const main = declarations(".item-main");
    expect(main).toMatch(/display:\s*flex\s*;/);
    expect(main).toMatch(/min-width:\s*0\s*;/);
    expect(main).toMatch(/overflow:\s*hidden\s*;/);
    expect(main).toMatch(/white-space:\s*nowrap\s*;/);
  });

  it("cuts the name, the word and the detail with an ellipsis rather than letting them spill", () => {
    for (const selector of [".item-name", ".item-main .word", ".item-main .caption"]) {
      const rule = declarations(selector);
      expect(rule, selector).toMatch(/min-width:\s*0\s*;/);
      expect(rule, selector).toMatch(/overflow:\s*hidden\s*;/);
      expect(rule, selector).toMatch(/text-overflow:\s*ellipsis\s*;/);
    }
  });

  describe("at the widths the verifier measured", () => {
    // jsdom lays nothing out, so the rows are laid out from the CSS text, with the widths the real window drew.
    const BODY = 13;
    const GAP = 0.3 * BODY;
    /** The main and the where column of a wide row together, at the narrowest window: 140.2 px was 1.3fr of it. */
    const WIDE_AT_960 = (140.2 * 2.3) / 1.3;
    /** The same at 1280 px, where a Job line of Needs you drew its main column 476 px wide. */
    const WIDE_AT_1280 = (476 * 2.3) / 1.3;

    /** The share of a wide row the main column takes, read from the fractions of `.item-select-wide`. */
    function mainColumn(wide: number): number {
      const fractions = [...declarations(".item-select-wide").matchAll(/minmax\(0, ([\d.]+)fr\)/g)].map(
        (found) => Number(found[1]),
      );
      const [main = 0, where = 0] = fractions;
      return (wide * main) / (main + where);
    }

    /** How much of a `flex` shorthand shrinks: its second number, 0 for `none`. */
    function shrinks(selector: string): boolean {
      const flex = /flex:\s*([^;]+);/.exec(declarations(selector))?.[1]?.trim() ?? "0 1 auto";
      return flex !== "none" && flex.split(/\s+/)[1] !== "0";
    }

    /** The largest width the name may take in a main column `width` wide, from `max(P%, 100% - Nem)`, or all of it. */
    function nameCap(width: number): number {
      const found = /max-width:\s*max\(([\d.]+)%, 100% - ([\d.]+)em\)\s*;/.exec(
        declarations(".item-main .item-name"),
      );
      return found === null
        ? width
        : Math.max((width * Number(found[1])) / 100, width - Number(found[2]) * BODY);
    }

    /** Lays a name `name` px wide and a word `word` px wide out on the line of a main column `width` wide. */
    function line(width: number, name: number, word: number): { name: number; word: number } {
      if (!shrinks(".item-main .word")) {
        // The word keeps its width, the name takes what is left.
        return { name: Math.max(0, Math.min(name, nameCap(width), width - word - GAP)), word };
      }
      const shown = Math.min(name, nameCap(width));
      return { name: shown, word: Math.max(0, Math.min(word, width - shown - GAP)) };
    }

    it("gives the name of a Foreign process of Needs you a readable width at 960 px, before its word and its Worktree", () => {
      // procly-test-server, started outside Procly, acme-web-login: the line that read only its word and its Worktree.
      const width = mainColumn(WIDE_AT_960);
      const where = WIDE_AT_960 - width;
      const shown = line(width, 122, 133);

      expect(shown.name).toBeGreaterThanOrEqual(90);
      expect(shown.name).toBeGreaterThan(shown.word);
      expect(shown.name).toBeGreaterThan(where);
    });

    it("still shows the word of a Job whole beside its long command, at 960 and at 1280 px", () => {
      for (const wide of [WIDE_AT_960, WIDE_AT_1280]) {
        // exited 1, 46.3 px, beside a command of 789 px.
        const shown = line(mainColumn(wide), 789, 46.3);
        expect(shown.word, String(wide)).toBe(46.3);
        expect(shown.name, String(wide)).toBeGreaterThan(55);
      }
    });

    it("shows a short name and its word whole", () => {
      expect(line(mainColumn(WIDE_AT_960), 18, 44.2)).toEqual({ name: 18, word: 44.2 });
    });
  });

  it("gives the ports a slot of their own, so that the pills of a list end on the same edge", () => {
    expect(declarations(".item-ports")).toMatch(/justify-content:\s*flex-end\s*;/);
    expect(declarations(".item-ports")).toMatch(/min-width:\s*\d+px\s*;/);
  });
});

describe("the title line of a screen", () => {
  it("is as tall as the strip over the sidebar and separated from the list by a line", () => {
    const bar = declarations(".title-bar");
    expect(bar).toMatch(/display:\s*flex\s*;/);
    expect(bar).toMatch(/min-height:\s*52px\s*;/);
    expect(bar).toMatch(/border-bottom:\s*1px solid var\(--line\)\s*;/);
  });
});

describe("the hint of an empty inspector", () => {
  it("sits in the middle of the pane, in the faint colour", () => {
    const hint = declarations(".hint");
    expect(hint).toMatch(/align-items:\s*center\s*;/);
    expect(hint).toMatch(/justify-content:\s*center\s*;/);
    expect(hint).toMatch(/color:\s*var\(--faint\)\s*;/);
  });
});
