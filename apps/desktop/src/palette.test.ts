// @vitest-environment node
import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

const css = readFileSync(new URL("./styles.css", import.meta.url), "utf8");

/** The tokens of the spec: light, then dark. */
const SPEC: Record<string, [string, string]> = {
  "--bg": ["#e4e7ea", "#101215"],
  "--side": ["#f3f4f6", "#1b1e23"],
  "--panel": ["#ffffff", "#17191d"],
  "--raise": ["#eef0f2", "#23272d"],
  "--line": ["#dfe2e6", "#2a2f36"],
  "--text": ["#14161a", "#e8eaed"],
  "--muted": ["#50575f", "#a2a9b2"],
  "--faint": ["#646b74", "#858d97"],
  "--ok": ["#5c6b2f", "#8fa05f"],
  "--warn": ["#8f5a00", "#f0b042"],
  "--fail": ["#b42318", "#ff6b6b"],
  "--ended": ["#6f767e", "#7b828b"],
  "--accent": ["#007aff", "#0a84ff"],
  "--brand-lime": ["#d7f23c", "#d7f23c"],
  "--brand-lime-deep": ["#a8bf22", "#a8bf22"],
  "--brand-ink": ["#14161a", "#14161a"],
};

const TEXT_AND_STATE = ["--text", "--muted", "--faint", "--ok", "--warn", "--fail", "--ended"];

function tokens(block: string): Map<string, string> {
  return new Map(
    [...block.matchAll(/(--[\w-]+):\s*(#[0-9a-f]{6});/gi)].map((match) => [
      match[1] ?? "",
      (match[2] ?? "").toLowerCase(),
    ]),
  );
}

function palette(): { light: Map<string, string>; dark: Map<string, string> } {
  const [lightBlock = "", afterDark = ""] = css.split("@media (prefers-color-scheme: dark)");
  const darkBlock = afterDark.slice(0, afterDark.indexOf("@supports"));
  const light = tokens(lightBlock);
  // The dark block only restates what changes.
  const dark = new Map([...light, ...tokens(darkBlock)]);
  return { light, dark };
}

function luminance(hex: string): number {
  const channels = [1, 3, 5].map((start) => {
    const value = Number.parseInt(hex.slice(start, start + 2), 16) / 255;
    return value <= 0.03928 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * (channels[0] ?? 0) + 0.7152 * (channels[1] ?? 0) + 0.0722 * (channels[2] ?? 0);
}

function contrast(foreground: string, background: string): number {
  const [lighter, darker] = [luminance(foreground), luminance(background)].sort((a, b) => b - a);
  return ((lighter ?? 0) + 0.05) / ((darker ?? 0) + 0.05);
}

describe("the palette", () => {
  it("holds the tokens of the spec in both themes", () => {
    const { light, dark } = palette();
    for (const [name, [lightValue, darkValue]] of Object.entries(SPEC)) {
      expect(light.get(name), `${name} light`).toBe(lightValue);
      expect(dark.get(name), `${name} dark`).toBe(darkValue);
    }
  });

  it.each(["light", "dark"] as const)(
    "keeps every text and state token at 4.5:1 on the panel in the %s theme",
    (theme) => {
      const colours = palette()[theme];
      const panel = colours.get("--panel") ?? "";
      for (const name of TEXT_AND_STATE) {
        const ratio = contrast(colours.get(name) ?? "", panel);
        expect(ratio, `${name} on --panel, ${theme}`).toBeGreaterThanOrEqual(4.5);
      }
    },
  );

  it.each(["light", "dark"] as const)(
    "keeps the mark cut out of a warning triangle readable in the %s theme",
    (theme) => {
      const colours = palette()[theme];
      const cut = colours.get("--panel") ?? "";
      // 3:1 is the floor of WCAG 1.4.11 for a graphical object, the bar and the dot are drawn in the pane colour.
      expect(
        contrast(cut, colours.get("--warn") ?? ""),
        `--panel on --warn, ${theme}`,
      ).toBeGreaterThanOrEqual(3);
    },
  );

  it("draws the cross of a failure white in light, as the mockup does, and at 3:1 on the red disc in both themes", () => {
    const { light, dark } = palette();
    expect(light.get("--fail-cut")).toBe("#ffffff");
    for (const [theme, colours] of [
      ["light", light],
      ["dark", dark],
    ] as const) {
      // White on the dark red would fall to 2.78:1, so the dark theme draws the cross in the colour of the pane.
      const ratio = contrast(colours.get("--fail-cut") ?? "", colours.get("--fail") ?? "");
      expect(ratio, `--fail-cut on --fail, ${theme}`).toBeGreaterThanOrEqual(3);
    }
  });

  it("tints the picked row with the accent, lighter in light than in dark", () => {
    expect(css).toMatch(/\.item\[data-selected="true"\]\s*\{\s*background: var\(--selected\);/);
    expect(css).toMatch(/:root\s*\{[^}]*--selected: color-mix\(in srgb, var\(--accent\) 14%, transparent\)/);
    const dark = /@media \(prefers-color-scheme: dark\)\s*\{[\s\S]*?\n\}/.exec(css)?.[0] ?? "";
    expect(dark).toMatch(/--selected: color-mix\(in srgb, var\(--accent\) 22%, transparent\)/);
  });

  it.each([
    ["light", 0.14],
    ["dark", 0.22],
  ] as const)(
    "keeps the text and the state glyphs readable on the tint of the open sidebar entry in the %s theme",
    (theme, share) => {
      const colours = palette()[theme];
      const channel = (hex: string, at: number): number => Number.parseInt(hex.slice(at, at + 2), 16);
      const side = colours.get("--side") ?? "";
      const accent = colours.get("--accent") ?? "";
      // The tint is the accent over the sidebar, as `color-mix` blends them.
      const tint = `#${[1, 3, 5]
        .map((at) =>
          Math.round(channel(accent, at) * share + channel(side, at) * (1 - share))
            .toString(16)
            .padStart(2, "0"),
        )
        .join("")}`;
      for (const name of ["--text", "--muted"]) {
        expect(
          contrast(colours.get(name) ?? "", tint),
          `${name} on the open entry, ${theme}`,
        ).toBeGreaterThanOrEqual(4.5);
      }
      for (const name of ["--ok", "--warn", "--fail", "--ended"]) {
        // 3:1 is the floor of WCAG 1.4.11 for a graphical object.
        expect(
          contrast(colours.get(name) ?? "", tint),
          `${name} on the open entry, ${theme}`,
        ).toBeGreaterThanOrEqual(3);
      }
    },
  );

  it("falls back to the system accent where WebKit provides it", () => {
    expect(css).toMatch(
      /@supports \(color: -apple-system-control-accent\)\s*\{\s*:root\s*\{\s*--accent: -apple-system-control-accent;/,
    );
  });
});
