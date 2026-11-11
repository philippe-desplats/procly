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

describe("the Daemon line of the sidebar footer", () => {
  // jsdom loads no stylesheet and measures nothing, so the way the line holds on one row is read from the CSS text.
  it("is one row of the flex line, which lets its text shrink below its width", () => {
    expect(declarations(".daemon-line")).toMatch(/display:\s*flex\s*;/);
    expect(declarations(".daemon-line")).toMatch(/min-width:\s*0\s*;/);
    expect(declarations(".daemon-line-text")).toMatch(/min-width:\s*0\s*;/);
  });

  it("wraps the time under the words when it does not fit beside them, instead of cutting either", () => {
    const text = declarations(".daemon-line-text");
    expect(text).toMatch(/display:\s*flex\s*;/);
    expect(text).toMatch(/flex-wrap:\s*wrap\s*;/);
    // A wrapped time is a line of its own: nothing forces the words and the time onto one row.
    expect(text).not.toMatch(/white-space:\s*nowrap/);
  });

  it("keeps the words on one line, cut with an ellipsis only when they alone do not fit the line", () => {
    const words = declarations(".daemon-line-words");
    expect(words).toMatch(/white-space:\s*nowrap\s*;/);
    expect(words).toMatch(/max-width:\s*100%\s*;/);
    expect(words).toMatch(/min-width:\s*0\s*;/);
    expect(words).toMatch(/overflow:\s*hidden\s*;/);
    expect(words).toMatch(/text-overflow:\s*ellipsis\s*;/);
  });

  it("never cuts the time, which is the part that changes", () => {
    const time = declarations(".daemon-line-time");
    expect(time).toMatch(/flex:\s*none\s*;/);
    expect(time).toMatch(/white-space:\s*nowrap\s*;/);
  });

  it("keeps the glyph beside the first line, where a centered glyph would drift to the middle of 2 lines", () => {
    expect(declarations(".daemon-line")).toMatch(/align-items:\s*flex-start\s*;/);
    expect(declarations(".daemon-line .glyph")).toMatch(/margin-top:\s*2px\s*;/);
  });

  describe("whatever the font", () => {
    // Measured with AppKit at 11px in the system face of macOS: the words 135.1 px, the time 48.2 px.
    const WORDS = 135.1;
    const TIME = 48.2;
    const GLYPH = 12;
    const GAP = 7;
    const START = 8;
    const TIME_GAP = 3.3;
    const ROOM = 236 - 16;
    const side = (): number => ROOM - START - GLYPH - GAP;

    /** Whether the line is whole at a font `scale` times wider: on one row, or the time under the words. */
    function whole(scale: number): { rows: 1 | 2; fits: boolean } {
      const room = side();
      const words = WORDS * scale;
      const time = TIME * scale;
      const oneRow = words + TIME_GAP + time <= room;
      return { rows: oneRow ? 1 : 2, fits: oneRow || (words <= room && time <= room) };
    }

    it("is on one row in the system font of macOS, as the mockup draws it", () => {
      expect(whole(1)).toEqual({ rows: 1, fits: true });
    });

    it("is whole in a font 15 percent wider, the one WebKitGTK needed, with the time on a 2nd row", () => {
      expect(whole(1.15)).toEqual({ rows: 2, fits: true });
      expect(whole(1.3)).toEqual({ rows: 2, fits: true });
    });

    it("holds for a font up to 40 percent wider, where the words alone start to be cut", () => {
      expect(whole(1.4).fits).toBe(true);
      expect(whole(1.5).fits).toBe(false);
    });
  });
});
