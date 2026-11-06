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

describe("the place caption of a row", () => {
  // jsdom loads no stylesheet, so the way a long directory gives way to the command is read from the CSS text.
  it("is capped and cut with an ellipsis, so the name of the row keeps its width", () => {
    const where = declarations(".item-where");
    expect(where).toMatch(/min-width:\s*0\s*;/);
    expect(where).toMatch(/max-width:\s*40%\s*;/);
    expect(where).toMatch(/overflow:\s*hidden\s*;/);
    expect(where).toMatch(/text-overflow:\s*ellipsis\s*;/);
    expect(where).toMatch(/white-space:\s*nowrap\s*;/);
  });
});
