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

describe("the legend of a Worktree in the sidebar", () => {
  // jsdom loads no stylesheet, so the way the legend sits beside the label is read from the CSS text.
  it("sits right after the label, which gives way to it when the name is long", () => {
    expect(declarations(".row-line")).toMatch(/justify-content:\s*flex-start\s*;/);
    expect(declarations(".row-legend")).toMatch(/flex:\s*none\s*;/);
    expect(declarations(".row-legend")).toMatch(/white-space:\s*nowrap\s*;/);
  });

  it("is followed under the name by the detail of an entry, small and muted", () => {
    const detail = declarations(".row-detail");
    expect(detail).toMatch(/color:\s*var\(--muted\)\s*;/);
    expect(detail).toMatch(/font-size:\s*var\(--size-small\)\s*;/);
  });
});
