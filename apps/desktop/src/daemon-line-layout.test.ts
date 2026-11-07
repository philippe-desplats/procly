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
  it("is one row of the flex line, with the glyph kept whole beside it", () => {
    expect(declarations(".daemon-line")).toMatch(/display:\s*flex\s*;/);
    expect(declarations(".daemon-line")).toMatch(/min-width:\s*0\s*;/);
  });

  it("never wraps, and is cut with an ellipsis when the sidebar is too narrow for it", () => {
    const text = declarations(".daemon-line-text");
    expect(text).toMatch(/min-width:\s*0\s*;/);
    expect(text).toMatch(/overflow:\s*hidden\s*;/);
    expect(text).toMatch(/text-overflow:\s*ellipsis\s*;/);
    expect(text).toMatch(/white-space:\s*nowrap\s*;/);
  });
});
