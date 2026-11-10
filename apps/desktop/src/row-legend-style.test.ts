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
});

describe("the title of a group in the sidebar", () => {
  it("is strong and small, and sits on the text of the rows under it", () => {
    const title = declarations(".group-title");
    expect(title).toMatch(/font-weight:\s*600\s*;/);
    expect(title).toMatch(/font-size:\s*var\(--size-small\)\s*;/);
    // The 8px of padding of a row, so that the title and the labels under it start on the same edge.
    expect(title).toMatch(/padding:\s*14px 8px 4px\s*;/);
    expect(title).toMatch(/margin:\s*0\s*;/);
  });
});

describe("the sidebar", () => {
  it("leaves the Daemon line the 204 px it needs: 236 px wide, 8px at each side, 8px of padding in the line", () => {
    expect(declarations(".window")).toMatch(/236px minmax/);
    expect(declarations(".sidebar")).toMatch(/padding:\s*0 8px 8px\s*;/);
    expect(declarations(".daemon-line")).toMatch(/padding:\s*6px 8px 2px\s*;/);
  });
});
