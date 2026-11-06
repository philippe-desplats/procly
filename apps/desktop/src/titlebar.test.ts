// @vitest-environment node
import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

const css = readFileSync(new URL("./styles.css", import.meta.url), "utf8");

/** The declarations of the first rule that has exactly this selector. */
function rule(selector: string): string {
  const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const match = new RegExp(`(?:^|\\n)${escaped}\\s*\\{([^}]*)\\}`).exec(css);
  expect(match, `a rule for ${selector}`).not.toBeNull();
  return match?.[1] ?? "";
}

describe("the top of the sidebar", () => {
  it("is a strip tall enough to clear the traffic lights, which the first entry starts below", () => {
    const height = /height:\s*(\d+)px/.exec(rule(".drag-strip"));

    // The native traffic lights end near 32px from the top of the window.
    expect(Number(height?.[1])).toBeGreaterThanOrEqual(40);
  });

  it("stays at the top of the sidebar when the list of Worktrees scrolls", () => {
    expect(rule(".drag-strip")).toMatch(/position:\s*sticky/);
    expect(rule(".drag-strip")).toMatch(/top:\s*0/);
  });

  it("leaves no padding above the strip, so that it starts at the top edge of the window", () => {
    expect(rule(".sidebar")).toMatch(/padding-top:\s*0/);
  });
});
