/**
 * Draws the 3 template glyphs of the menu bar from their SVG sources: `bun scripts/export-glyphs.ts`.
 *
 * The sources in `glyphs/` ship as drawn. Each is exported at 36 px tall (18 pt at 2x, 3 px per unit of the 12x12
 * grid of the brand tile) into `src-tauri/icons/`, where the app embeds them. Run it again whenever a source changes
 * and commit the PNGs with it.
 */
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

import { Resvg } from "@resvg/resvg-js";

/** The folder of the SVG sources. */
export const GLYPHS = new URL("../glyphs/", import.meta.url);

/** The folder the app embeds the PNGs from. */
export const ICONS = new URL("../src-tauri/icons/", import.meta.url);

/** The height of every exported glyph, in pixels. */
const HEIGHT = 36;

/** Each source and the PNG it becomes, in the order the app names them. */
const EXPORTS = [
  ["mark.svg", "tray-mark.png"],
  ["mark-needs-you.svg", "tray-mark-needs-you.png"],
  ["outline.svg", "tray-outline.png"],
] as const;

/** Exports every glyph of `sources` as a PNG into `out`, and returns the paths it wrote. */
export async function exportGlyphs(sources: URL, out: string | URL): Promise<string[]> {
  const folder = typeof out === "string" ? out : fileURLToPath(out);
  await mkdir(folder, { recursive: true });
  const written: string[] = [];
  for (const [source, png] of EXPORTS) {
    const svg = await readFile(new URL(source, sources));
    const rendered = new Resvg(svg, { fitTo: { mode: "height", value: HEIGHT } }).render();
    const path = join(folder, png);
    await writeFile(path, rendered.asPng());
    written.push(path);
  }
  return written;
}

if (import.meta.main) {
  for (const path of await exportGlyphs(GLYPHS, ICONS)) {
    console.log(path);
  }
}
