// @vitest-environment node
import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

const css = readFileSync(new URL("./styles.css", import.meta.url), "utf8");

/** The declarations of the first rule whose selector is exactly `selector`, outside any at-rule. */
function rule(selector: string, source: string = css): string | null {
  const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const match = new RegExp(`(?:^|\\})\\s*${escaped}\\s*\\{([^}]*)\\}`, "m").exec(source);
  return match?.[1] ?? null;
}

/** The body of the `@media (prefers-reduced-motion: reduce)` block. */
function reducedMotion(): string {
  const start = css.indexOf("@media (prefers-reduced-motion: reduce)");
  expect(start).toBeGreaterThan(-1);
  const open = css.indexOf("{", start);
  let depth = 0;
  for (let at = open; at < css.length; at += 1) {
    if (css[at] === "{") depth += 1;
    if (css[at] === "}") depth -= 1;
    if (depth === 0) return css.slice(open + 1, at);
  }
  throw new Error("the reduced motion block never closes");
}

describe("the glyph stylesheet", () => {
  it("spins the restarting ring in a loop", () => {
    expect(css).toMatch(/@keyframes spin\s*\{[^}]*\{[^}]*rotate\(360deg\)/);
    expect(rule(".spin")).toMatch(/animation:\s*spin [\d.]+s linear infinite/);
  });

  it("stops the ring under prefers-reduced-motion", () => {
    expect(rule(".spin", reducedMotion())).toMatch(/animation:\s*none/);
  });

  it("draws the glyph of each tone in a token, never in a colour of its own", () => {
    for (const [tone, token] of [
      ["ok", "--ok"],
      ["warn", "--warn"],
      ["fail", "--fail"],
      ["ended", "--ended"],
      ["muted", "--muted"],
    ] as const) {
      expect(rule(`.tone-${tone} .glyph`)).toContain(`color: var(${token})`);
    }
  });

  it("colours the word of a failure and of a restart too, and leaves the other words grey", () => {
    expect(rule(".state.tone-fail")).toContain("color: var(--fail)");
    expect(rule(".state.tone-warn")).toContain("color: var(--warn)");
    expect(rule(".state.tone-ok")).toBeNull();
  });

  it("no longer draws the 8 px dot", () => {
    expect(css).not.toMatch(/\.dot\b/);
  });
});
