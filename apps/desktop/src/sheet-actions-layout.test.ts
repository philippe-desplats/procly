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

describe("the buttons of a sheet", () => {
  // jsdom loads no stylesheet, so the side the buttons sit on is read from the CSS text. The sheets put the button
  // that acts first and Cancel last, so that flex-end leaves Cancel at the right edge, next to the default focus.
  it("are pushed to the right edge, in the order the sheet writes them", () => {
    const actions = declarations(".actions");
    expect(actions).toMatch(/display:\s*flex\s*;/);
    expect(actions).toMatch(/justify-content:\s*flex-end\s*;/);
    expect(actions).not.toMatch(/flex-direction:\s*row-reverse/);
  });
});
