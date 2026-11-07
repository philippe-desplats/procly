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
  it("is one row of the flex line, which lets its text shrink below its width", () => {
    expect(declarations(".daemon-line")).toMatch(/display:\s*flex\s*;/);
    expect(declarations(".daemon-line")).toMatch(/min-width:\s*0\s*;/);
    expect(declarations(".daemon-line-text")).toMatch(/min-width:\s*0\s*;/);
  });

  it("never wraps, and a narrow sidebar cuts its words with an ellipsis", () => {
    expect(declarations(".daemon-line-text")).toMatch(/white-space:\s*nowrap\s*;/);
    const words = declarations(".daemon-line-words");
    expect(words).toMatch(/min-width:\s*0\s*;/);
    expect(words).toMatch(/overflow:\s*hidden\s*;/);
    expect(words).toMatch(/text-overflow:\s*ellipsis\s*;/);
  });

  it("never cuts the time, which is the part that changes", () => {
    expect(declarations(".daemon-line-time")).toMatch(/flex:\s*none\s*;/);
  });
});
