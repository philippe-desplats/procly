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
    expect(declarations(".item-select")).toMatch(
      /grid-template-columns:\s*14px minmax\(auto, 1fr\) auto\s*;/,
    );
  });

  it("has a column for where it runs when it says so, between the name and the ports, narrower than the name", () => {
    expect(declarations(".item-select-wide")).toMatch(
      /grid-template-columns:\s*14px minmax\(auto, 2fr\) minmax\(0, 1fr\) auto\s*;/,
    );
  });

  it("keeps the name and its word on one line, and the word of the state whole, never cut nor wrapped", () => {
    const main = declarations(".item-main");
    expect(main).toMatch(/display:\s*flex\s*;/);
    expect(main).toMatch(/white-space:\s*nowrap\s*;/);
    // Hidden, the main column could be squeezed below its word: visible, the word sets the least width of the column.
    expect(main).not.toMatch(/overflow:\s*hidden/);
    expect(declarations(".item-main .word")).toMatch(/flex:\s*none\s*;/);
  });

  it("cuts the name and the detail on one line with an ellipsis, and lets them give way below their width", () => {
    for (const selector of [".item-main .item-name", ".item-main .caption"]) {
      const rule = declarations(selector);
      expect(rule, selector).toMatch(/-webkit-line-clamp:\s*1\s*;/);
      expect(rule, selector).toMatch(/overflow:\s*hidden\s*;/);
      expect(rule, selector).toMatch(/overflow-wrap:\s*anywhere\s*;/);
      expect(rule, selector).toMatch(/white-space:\s*normal\s*;/);
    }
  });

  describe("at the widths the verifier measured", () => {
    // jsdom lays nothing out, so each row is laid out from the CSS text, as a grid then a flex line, with the widths
    // the real window drew at 960 and 1280 px.
    const BODY = 13;
    /** The gap of the flex line of the name, 0.3em. */
    const GAP = 0.3 * BODY;
    /** The gap between the columns of a row. */
    const COLUMN_GAP = 10;
    const GLYPH = 14;
    /** The inner width of a row of the center: 348 px less its padding at 960 px, 660 - 32 - 20 at 1280 px. */
    const ROW = { 960: 328, 1280: 608 };
    /** The last column: a Port pill, or the id of a Job, as the main columns of 165.33 and 123.72 px at 960 px tell. */
    const END = { port: 36, jobId: 98.42 };

    interface Text {
      /** Its width on one line. */
      width: number;
      /** Its length in characters, which the floor of a name is counted in. */
      chars: number;
    }

    function rule(selector: string): string {
      return declarations(selector);
    }

    /** The least width of a name, from `min(calc(var(--name-chars, 0) * Aem), Bem)`, 0 when there is none. */
    function floor(name: Text): number {
      const found = /min-width:\s*min\(calc\(var\(--name-chars, 0\) \* ([\d.]+)em\), ([\d.]+)em\)\s*;/.exec(
        rule(".item-main .item-name"),
      );
      return found === null ? 0 : Math.min(name.chars * Number(found[1]), Number(found[2])) * BODY;
    }

    /** What a text gives the least width of its line: nothing past its floor when it may break anywhere, else all of it. */
    function least(selector: string, text: Text, own: number): number {
      return /overflow-wrap:\s*anywhere/.test(rule(selector)) ? own : text.width;
    }

    /** The cap of a name, from `max-width: max(P%, 100% - Nem)`, or none. */
    function cap(width: number): number {
      const found = /max-width:\s*max\(([\d.]+)%, 100% - ([\d.]+)em\)\s*;/.exec(
        rule(".item-main .item-name"),
      );
      return found === null
        ? Infinity
        : Math.max((width * Number(found[1])) / 100, width - Number(found[2]) * BODY);
    }

    /** Whether the `flex` shorthand of `selector` lets it shrink. */
    function shrinks(selector: string): boolean {
      const flex = /flex:\s*([^;]+);/.exec(rule(selector))?.[1]?.trim() ?? "0 1 auto";
      return flex !== "none" && flex.split(/\s+/)[1] !== "0";
    }

    /** Lays a row out, and returns what its name, its word, its detail and where it runs show. */
    function row({
      at,
      wide,
      end,
      name,
      word,
      detail,
    }: {
      at: 960 | 1280;
      wide: boolean;
      end: number;
      name: Text;
      word: number;
      detail?: Text;
    }): { name: number; word: number; detail: number; where: number; overflows: boolean } {
      const columns =
        /grid-template-columns:\s*([^;]+);/.exec(rule(wide ? ".item-select-wide" : ".item-select"))?.[1] ??
        "";
      const tracks = [...columns.matchAll(/minmax\((auto|0), ([\d.]+)fr\)/g)].map((found) => ({
        auto: found[1] === "auto",
        fr: Number(found[2]),
      }));
      const [main = { auto: false, fr: 1 }, where] = tracks;
      const room = ROW[at] - GLYPH - end - COLUMN_GAP * (wide ? 3 : 2);
      // A main column whose least width is auto keeps at least the least width of its line, when that line is visible.
      const hidden = /overflow:\s*hidden/.test(rule(".item-main"));
      const nameFloor = floor(name);
      const lineLeast =
        least(".item-main .item-name", name, nameFloor) +
        (detail === undefined ? 0 : least(".item-main .caption", detail, 0) + GAP) +
        (shrinks(".item-main .word") ? 0 : word) +
        GAP;
      const share = where === undefined ? room : (room * main.fr) / (main.fr + where.fr);
      const column = Math.min(room, Math.max(share, main.auto && !hidden ? lineLeast : 0));
      const overflows = main.auto && !hidden && lineLeast > room;
      const detailWidth = detail?.width ?? 0;
      const gaps = GAP * (detail === undefined ? 1 : 2);
      if (shrinks(".item-main .word")) {
        const shown = Math.min(name.width, cap(column));
        const wordShown = Math.max(0, Math.min(word, column - shown - gaps));
        return { name: shown, word: wordShown, detail: 0, where: room - column, overflows };
      }
      // The word keeps its width, the name and the detail share what is left, the name never below its floor.
      const left = column - word - gaps;
      const over = Math.max(0, name.width + detailWidth - left);
      const nameShown = Math.max(
        Math.min(
          name.width,
          nameFloor > 0
            ? Math.max(nameFloor, name.width - (over * name.width) / (name.width + detailWidth))
            : left,
        ),
        Math.min(nameFloor, name.width),
      );
      const detailShown = Math.max(0, Math.min(detailWidth, left - nameShown));
      return {
        name: Math.min(nameShown, Math.max(0, left)),
        word: hidden ? Math.min(word, column) : word,
        detail: detailShown,
        where: room - column,
        overflows,
      };
    }

    const command = { width: 788.78, chars: 115 };
    const exited = 46.32;

    it("shows the word of a Job of Needs you whole beside its long command, at 960 and 1280 px", () => {
      for (const at of [960, 1280] as const) {
        const shown = row({ at, wide: true, end: END.jobId, name: command, word: exited });
        expect(shown.word, String(at)).toBe(exited);
        expect(shown.name, String(at)).toBeGreaterThanOrEqual(60);
        expect(shown.overflows, String(at)).toBe(false);
      }
    });

    it("shows the word of a Job of Outside Repositories whole beside its command and its directory", () => {
      const shown = row({
        at: 960,
        wide: true,
        end: END.jobId,
        name: { width: 387.59, chars: 60 },
        word: 45.89,
      });

      expect(shown.word).toBe(45.89);
      expect(shown.name).toBeGreaterThanOrEqual(60);
    });

    it("gives a Foreign process of Needs you its whole word and a readable name at 960 px, its Worktree giving way", () => {
      const shown = row({ at: 960, wide: true, end: END.port, name: { width: 122.4, chars: 18 }, word: 133 });

      expect(shown.word).toBe(133);
      expect(shown.name).toBeGreaterThanOrEqual(65);
      expect(shown.where).toBeLessThan(shown.name);
      expect(shown.overflows).toBe(false);
    });

    it("keeps started outside Procly whole on the Worktree screen, the pid giving way before the name", () => {
      const shown = row({
        at: 960,
        wide: false,
        end: END.port,
        name: { width: 122.4, chars: 18 },
        word: 133,
        detail: { width: 37.2, chars: 9 },
      });

      expect(shown.word).toBe(133);
      expect(shown.name).toBeGreaterThanOrEqual(65);
      expect(shown.overflows).toBe(false);
    });

    it("shows a short name and its word whole, the floor of the name never wider than the name", () => {
      const api = { width: 18, chars: 3 };

      expect(floor(api)).toBeLessThanOrEqual(api.width);
      expect(row({ at: 960, wide: true, end: END.port, name: api, word: 44.19 })).toMatchObject({
        name: 18,
        word: 44.19,
      });
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
