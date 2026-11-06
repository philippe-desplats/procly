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

describe("the count on a sidebar entry", () => {
  // jsdom loads no stylesheet, so the look of the count is read from the CSS text.
  it("is small, muted and set in tabular figures, as the mockup draws it", () => {
    const count = declarations(".row-count");
    expect(count).toMatch(/font-size:\s*var\(--size-small\)\s*;/);
    expect(count).toMatch(/color:\s*var\(--muted\)\s*;/);
    expect(count).toMatch(/font-variant-numeric:\s*tabular-nums\s*;/);
  });

  it("keeps its own colour on the open entry, which is tinted and not filled", () => {
    expect(declarations('.row[aria-current="true"] .row-count')).toBe("");
  });
});
