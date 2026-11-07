/**
 * Draws the glyphs of the menu bar from their SVG sources: `bun scripts/export-glyphs.ts`.
 *
 * The sources in `glyphs/` ship as drawn. The 3 template glyphs of the icon are exported at 36 px tall (18 pt at 2x,
 * 3 px per unit of the 12x12 grid of the brand tile), and the 2 state glyphs the lines of Needs you lead with at 32 px
 * (16 pt at 2x, the size of an image in a menu item), into `src-tauri/icons/`, where the app embeds them. Run it again
 * whenever a source changes and commit the PNGs with it.
 */
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

import { Resvg } from "@resvg/resvg-js";

/** The folder of the SVG sources. */
export const GLYPHS = new URL("../glyphs/", import.meta.url);

/** The folder the app embeds the PNGs from. */
export const ICONS = new URL("../src-tauri/icons/", import.meta.url);

/** The height of a glyph of the icon, in pixels. */
const ICON_HEIGHT = 36;

/** The height of a state glyph in a menu item, in pixels. */
const ITEM_HEIGHT = 32;

/** Each source, the PNG it becomes and its height, in the order the app names them. */
const EXPORTS = [
  ["mark.svg", "tray-mark.png", ICON_HEIGHT],
  ["mark-needs-you.svg", "tray-mark-needs-you.png", ICON_HEIGHT],
  ["outline.svg", "tray-outline.png", ICON_HEIGHT],
  ["menu-failure.svg", "menu-failure.png", ITEM_HEIGHT],
  ["menu-foreign.svg", "menu-foreign.png", ITEM_HEIGHT],
] as const;

/** Exports every glyph of `sources` as a PNG into `out`, and returns the paths it wrote. */
export async function exportGlyphs(sources: URL, out: string | URL): Promise<string[]> {
  const folder = typeof out === "string" ? out : fileURLToPath(out);
  await mkdir(folder, { recursive: true });
  const written: string[] = [];
  for (const [source, png, height] of EXPORTS) {
    const svg = await readFile(new URL(source, sources));
    const rendered = new Resvg(svg, { fitTo: { mode: "height", value: height } }).render();
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
