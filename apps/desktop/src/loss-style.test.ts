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

describe("the loss line", () => {
  // jsdom loads no stylesheet, so the colour of a loss is read from the CSS text. The contrast of --fail on --panel
  // in both themes is held by palette.test.ts, so the class only has to use that token and nothing else.
  it("is bold and takes the failure token, never a literal colour", () => {
    const loss = declarations(".loss");
    expect(loss).toMatch(/color:\s*var\(--fail\)\s*;/);
    expect(loss).toMatch(/font-weight:\s*600\s*;/);
  });

  it("comes after .muted and .caption, so a muted line that loses data still shows red", () => {
    expect(css.indexOf("\n.loss {")).toBeGreaterThan(css.indexOf("\n.muted {"));
    expect(css.indexOf("\n.loss {")).toBeGreaterThan(css.indexOf("\n.caption {"));
  });
});
