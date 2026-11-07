// @vitest-environment node
import { mkdtemp, readFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { basename, join } from "node:path";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

import { exportGlyphs, GLYPHS, ICONS } from "./export-glyphs";

const PNG_SIGNATURE = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);

// Rasterizing the glyphs takes over 5 s on the macOS CI runner.
const EXPORT_TIMEOUT_MS = 30_000;

/** The size a PNG declares in its header, and whether it is a PNG at all. */
async function header(path: string): Promise<{ png: boolean; width: number; height: number }> {
  const bytes = await readFile(path);
  return {
    png: bytes.subarray(0, 8).equals(PNG_SIGNATURE),
    width: bytes.readUInt32BE(16),
    height: bytes.readUInt32BE(20),
  };
}

describe("the glyph export script", () => {
  it(
    "draws the 3 template PNGs of the tray from the SVG sources, each 36 px tall",
    async () => {
      const out = await mkdtemp(join(tmpdir(), "procly-glyphs-"));

      const written = await exportGlyphs(GLYPHS, out);

      expect(written.map((path) => basename(path)).slice(0, 3)).toEqual([
        "tray-mark.png",
        "tray-mark-needs-you.png",
        "tray-outline.png",
      ]);
      const sizes = await Promise.all(written.slice(0, 3).map(header));
      expect(sizes.map((size) => size.png)).toEqual([true, true, true]);
      expect(sizes.map((size) => size.height)).toEqual([36, 36, 36]);
      // The mark and its outline are 12 units wide on a 3 px grid, the bite widens the mark to 13.85 units.
      expect(sizes[0]?.width).toBe(36);
      expect(sizes[1]?.width).toBeGreaterThanOrEqual(41);
      expect(sizes[1]?.width).toBeLessThanOrEqual(42);
      expect(sizes[2]?.width).toBe(36);
    },
    EXPORT_TIMEOUT_MS,
  );

  it(
    "draws the 2 state glyphs of the lines of Needs you in the menu bar menu, each 32 px square",
    async () => {
      const out = await mkdtemp(join(tmpdir(), "procly-glyphs-"));

      const written = await exportGlyphs(GLYPHS, out);

      expect(written.map((path) => basename(path)).slice(3)).toEqual(["menu-failure.png", "menu-foreign.png"]);
      const sizes = await Promise.all(written.slice(3).map(header));
      expect(sizes).toEqual([
        { png: true, width: 32, height: 32 },
        { png: true, width: 32, height: 32 },
      ]);
    },
    EXPORT_TIMEOUT_MS,
  );

  it(
    "agrees in size with the PNGs the app ships, so that a glyph edited without exporting is caught",
    async () => {
      const out = await mkdtemp(join(tmpdir(), "procly-glyphs-"));

      const written = await exportGlyphs(GLYPHS, out);

      for (const path of written) {
        const shipped = join(fileURLToPath(ICONS), basename(path));
        expect(await header(shipped)).toEqual(await header(path));
      }
    },
    EXPORT_TIMEOUT_MS,
  );
});
