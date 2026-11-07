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

describe("the title line of a Worktree", () => {
  // jsdom loads no stylesheet, so the layout is read from the CSS text.
  it("gives way to the buttons when the window narrows, and cuts the path with an ellipsis rather than wrapping it", () => {
    expect(declarations(".worktree-title")).toMatch(/flex:\s*1\s*;/);
    expect(declarations(".worktree-title")).toMatch(/min-width:\s*0\s*;/);
    const path = declarations(".worktree-path");
    expect(path).toMatch(/overflow:\s*hidden\s*;/);
    expect(path).toMatch(/text-overflow:\s*ellipsis\s*;/);
    expect(path).toMatch(/white-space:\s*nowrap\s*;/);
  });

  it("cuts the name of the folder the same way, and keeps the legend of the main Worktree whole", () => {
    const name = declarations(".worktree-head .title");
    expect(name).toMatch(/text-overflow:\s*ellipsis\s*;/);
    expect(name).toMatch(/white-space:\s*nowrap\s*;/);
    expect(declarations(".worktree-legend")).toMatch(/flex:\s*none\s*;/);
  });

  it("keeps the buttons that open the Worktree at their natural width", () => {
    expect(declarations(".worktree-head .button")).toMatch(/flex:\s*none\s*;/);
  });
});
