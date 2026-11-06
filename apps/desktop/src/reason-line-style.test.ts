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

describe("the reason line under a Worktree", () => {
  // jsdom loads no stylesheet, so the look of the line is read from the CSS text.
  it("is set in the 11 px size and the muted colour, as the mockup draws it", () => {
    expect(declarations(":root")).toMatch(/--size-small:\s*11px\s*;/);
    const line = declarations(".row-reason");
    expect(line).toMatch(/font-size:\s*var\(--size-small\)\s*;/);
    expect(line).toMatch(/color:\s*var\(--muted\)\s*;/);
  });

  it("is cut with an ellipsis at the end of the row, never wrapped", () => {
    const text = declarations(".row-reason-text");
    expect(text).toMatch(/min-width:\s*0\s*;/);
    expect(text).toMatch(/overflow:\s*hidden\s*;/);
    expect(text).toMatch(/text-overflow:\s*ellipsis\s*;/);
    expect(text).toMatch(/white-space:\s*nowrap\s*;/);
  });

  it("keeps its own colours on the open entry, so that the cross of a failure still shows", () => {
    expect(declarations('.row[aria-current="true"] .row-reason')).toBe("");
    expect(declarations('.row[aria-current="true"] .row-reason .glyph')).toBe("");
  });
});
