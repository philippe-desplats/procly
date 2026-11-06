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

describe("the copy box layout", () => {
  // jsdom loads no stylesheet, so the wrapping that keeps Copy inside a sheet is read from the CSS text.
  it("lets the command shrink and wrap, so a long path never pushes Copy out", () => {
    const code = declarations(".copybox code");
    expect(code).toMatch(/min-width:\s*0\s*;/);
    expect(code).toMatch(/flex:\s*1\s*;/);
    expect(code).toMatch(/white-space:\s*pre-wrap\s*;/);
    expect(code).toMatch(/overflow-wrap:\s*anywhere\s*;/);
  });

  it("keeps the Copy button at its natural size", () => {
    expect(declarations(".copybox .button")).toMatch(/flex:\s*none\s*;/);
  });
});
