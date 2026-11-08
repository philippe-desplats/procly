// @vitest-environment node
import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

const css = readFileSync(new URL("./styles.css", import.meta.url), "utf8");

/** Reads the declarations of the first rule of `text` whose selector is exactly the one asked for. */
function declarationsIn(text: string): (selector: string) => string {
  return (selector) => {
    const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    const match = new RegExp(`(?:^|\\})\\s*${escaped}\\s*\\{([^}]*)\\}`, "m").exec(text);
    return match?.[1] ?? "";
  };
}

/** The declarations of the first rule of the stylesheet whose selector is exactly `selector`. */
const declarations = declarationsIn(css);

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
    // A least width of 0 would take the line out of the least width of the column, and the word would leave it.
    expect(main).not.toMatch(/min-width:\s*0\s*;/);
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
    /** The size of a name set in the monospace face, a command or the name of a Foreign process. */
    const SMALL = 11;
    /** The gap of the flex line of the name, 0.3em of the body size. */
    const GAP = 0.3 * BODY;
    /** The gap between the columns of a row. */
    const COLUMN_GAP = 10;
    const GLYPH = 14;
    /** The inner width of a row of the center: 348 px less its padding at 960 px, 660 - 32 - 20 at 1280 px. */
    const ROW = { 960: 328, 1280: 608 };
    /**
     * The last column: a Port pill, or the id of a Job, as the main columns of 165.33 and 123.72 px at 960 px tell, or
     * the id and the time of a Job on the Worktree screen, as its command cut at 128.38 px at 960 px tells.
     */
    const END = { port: 36, jobId: 98.42, worktreeJob: 115.39 };

    interface Text {
      /** Its width on one line. */
      width: number;
      /** Its length in characters, which the floor of a name is counted in. */
      chars: number;
      /** Whether it is set in the monospace face, at the small size. */
      mono?: boolean;
    }

    type Rules = (selector: string) => string;

    /** The least width of a name, from `min(calc(var(--name-chars, 0) * Aem), Bem)` of its own size, 0 when none. */
    function floor(name: Text, rules: Rules = declarations): number {
      const found = /min-width:\s*min\(calc\(var\(--name-chars, 0\) \* ([\d.]+)em\), ([\d.]+)em\)\s*;/.exec(
        rules(".item-main .item-name"),
      );
      const size = name.mono === true ? SMALL : BODY;
      return found === null ? 0 : Math.min(name.chars * Number(found[1]), Number(found[2])) * size;
    }

    /** What a text gives the least width of its line: nothing past its floor when it may break anywhere, else all of it. */
    function least(rules: Rules, selector: string, text: Text, own: number): number {
      return /overflow-wrap:\s*anywhere/.test(rules(selector)) ? own : text.width;
    }

    /** Whether the `flex` shorthand of `selector` lets it shrink. */
    function shrinks(rules: Rules, selector: string): boolean {
      const flex = /flex:\s*([^;]+);/.exec(rules(selector))?.[1]?.trim() ?? "0 1 auto";
      return flex !== "none" && flex.split(/\s+/)[1] !== "0";
    }

    /**
     * Lays a row out, and returns its main column, what its name, its word, its detail and where it runs show, and
     * whether its line spills out of the main column, over where it runs.
     */
    function row({
      at,
      wide,
      end,
      name,
      word,
      detail,
      rules = declarations,
    }: {
      at: 960 | 1280;
      wide: boolean;
      end: number;
      name: Text;
      word: number;
      detail?: Text;
      rules?: Rules;
    }): { column: number; name: number; word: number; detail: number; where: number; spills: boolean } {
      const columns =
        /grid-template-columns:\s*([^;]+);/.exec(rules(wide ? ".item-select-wide" : ".item-select"))?.[1] ??
        "";
      const tracks = [...columns.matchAll(/minmax\((auto|0), ([\d.]+)fr\)/g)].map((found) => ({
        auto: found[1] === "auto",
        fr: Number(found[2]),
      }));
      const [main = { auto: false, fr: 1 }, where] = tracks;
      const room = ROW[at] - GLYPH - end - COLUMN_GAP * (wide ? 3 : 2);
      // A main column whose least width is auto keeps at least the least width of its line, unless the line is clipped
      // or declares a least width of 0, which leaves the column its share of the fr alone.
      const line = rules(".item-main");
      const contributes = main.auto && !/overflow:\s*hidden/.test(line) && !/min-width:\s*0\s*;/.test(line);
      const wordShrinks = shrinks(rules, ".item-main .word");
      const nameFloor = floor(name, rules);
      const gaps = GAP * (detail === undefined ? 1 : 2);
      const lineLeast =
        least(rules, ".item-main .item-name", name, nameFloor) +
        (detail === undefined ? 0 : least(rules, ".item-main .caption", detail, 0)) +
        (wordShrinks ? 0 : word) +
        gaps;
      const share = where === undefined ? room : (room * main.fr) / (main.fr + where.fr);
      const column = Math.min(room, Math.max(share, contributes ? lineLeast : 0));
      // The word keeps its width, the name and the detail shrink alike from what they need, the name never below its
      // floor, so a line wider than its column leaves it.
      const detailWidth = detail?.width ?? 0;
      const wordShown = wordShrinks ? Math.max(0, Math.min(word, column - nameFloor - gaps)) : word;
      const left = column - wordShown - gaps;
      const over = Math.max(0, name.width + detailWidth - left);
      const nameShown = Math.max(
        Math.min(name.width, name.width - (over * name.width) / (name.width + detailWidth)),
        Math.min(nameFloor, name.width),
      );
      const detailShown = Math.max(0, Math.min(detailWidth, left - nameShown));
      return {
        column,
        name: nameShown,
        word: wordShown,
        detail: detailShown,
        where: room - column,
        spills: nameShown + detailShown + gaps + wordShown > column + 0.01,
      };
    }

    const command = { width: 788.78, chars: 115, mono: true };
    const exited = 46.32;
    const foreign = { width: 122.41, chars: 18, mono: true };
    const outside = 133.02;

    it("shows the word of a Job of Needs you whole beside its long command, at 960 and 1280 px", () => {
      for (const at of [960, 1280] as const) {
        const shown = row({ at, wide: true, end: END.jobId, name: command, word: exited });
        expect(shown.word, String(at)).toBe(exited);
        expect(shown.name, String(at)).toBeGreaterThanOrEqual(70);
        expect(shown.spills, String(at)).toBe(false);
      }
    });

    it("shows the word of a Job of a Worktree whole beside its command, at 960 and 1280 px", () => {
      for (const at of [960, 1280] as const) {
        const shown = row({ at, wide: false, end: END.worktreeJob, name: command, word: exited });
        expect(shown.word, String(at)).toBe(exited);
        expect(shown.name, String(at)).toBeGreaterThanOrEqual(70);
        expect(shown.spills, String(at)).toBe(false);
      }
    });

    it("shows the word of a Job of Outside Repositories whole beside its command and its directory", () => {
      for (const at of [960, 1280] as const) {
        const shown = row({
          at,
          wide: true,
          end: END.jobId,
          name: { width: 387.59, chars: 60, mono: true },
          word: 45.89,
        });

        expect(shown.word, String(at)).toBe(45.89);
        expect(shown.name, String(at)).toBeGreaterThanOrEqual(70);
        expect(shown.spills, String(at)).toBe(false);
      }
    });

    it("gives a Foreign process of Needs you its whole word, inside its column, and a readable name at 960 px", () => {
      const shown = row({ at: 960, wide: true, end: END.port, name: foreign, word: outside });

      expect(shown.word).toBe(outside);
      expect(shown.spills).toBe(false);
      expect(shown.name).toBeGreaterThanOrEqual(70);
      // Its Worktree gives way first.
      expect(shown.where).toBeLessThan(shown.name);
    });

    it("shows a Foreign process of Needs you whole at 1280 px", () => {
      const shown = row({ at: 1280, wide: true, end: END.port, name: foreign, word: outside });

      expect(shown).toMatchObject({ name: foreign.width, word: outside, spills: false });
    });

    it("keeps started outside Procly whole on the Worktree screen, the pid giving way beside the name", () => {
      for (const at of [960, 1280] as const) {
        const shown = row({
          at,
          wide: false,
          end: END.port,
          name: foreign,
          word: outside,
          detail: { width: 52.52, chars: 9 },
        });

        expect(shown.word, String(at)).toBe(outside);
        expect(shown.name, String(at)).toBeGreaterThanOrEqual(70);
        expect(shown.spills, String(at)).toBe(false);
      }
    });

    it("shows a short name and its word whole, the floor of the name never wider than the name", () => {
      const api = { width: 18, chars: 3 };

      expect(floor(api)).toBeLessThanOrEqual(api.width);
      expect(row({ at: 960, wide: true, end: END.port, name: api, word: 44.19 })).toMatchObject({
        name: 18,
        word: 44.19,
        spills: false,
      });
    });

    it("lays the rows out as the app measured them on the stylesheet that let the word spill", () => {
      // The rules of the row as fa04c52 left them, which the verifier measured in the real window at 960 px: a main
      // column of 165.33 px for the Foreign process of Needs you, its name at 60.5 px, its word over where it runs, and
      // the commands of the Jobs cut at 73.5 and 73.92 px.
      const before = declarationsIn(`
.item-select {
  grid-template-columns: 14px minmax(auto, 1fr) auto;
}
.item-select-wide {
  grid-template-columns: 14px minmax(auto, 2fr) minmax(0, 1fr) auto;
}
.item-main {
  display: flex;
  min-width: 0;
  white-space: nowrap;
}
.item-main .item-name {
  flex: 0 1 auto;
  min-width: min(calc(var(--name-chars, 0) * 0.4em), 5.5em);
  overflow-wrap: anywhere;
}
.item-main .caption {
  flex: 0 1 auto;
  min-width: 0;
  overflow-wrap: anywhere;
}
.item-main .word {
  flex: none;
}
`);

      const needsYou = row({
        at: 960,
        wide: true,
        end: END.port,
        name: foreign,
        word: outside,
        rules: before,
      });
      expect(needsYou.column).toBeCloseTo(165.33, 2);
      expect(needsYou.name).toBeCloseTo(60.5, 2);
      expect(needsYou.spills).toBe(true);

      const job = row({ at: 960, wide: true, end: END.jobId, name: command, word: exited, rules: before });
      expect(job.column).toBeCloseTo(123.72, 2);
      expect(job.name).toBeCloseTo(73.5, 1);

      const outsideJob = row({
        at: 960,
        wide: true,
        end: END.jobId,
        name: { width: 387.59, chars: 60, mono: true },
        word: 45.89,
        rules: before,
      });
      expect(outsideJob.name).toBeCloseTo(73.92, 1);
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
