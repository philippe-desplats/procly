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

describe("the state screen layout", () => {
  // jsdom loads no stylesheet, so how the column is centered is read from the CSS text.
  it("centers one column of content on the whole window", () => {
    const screen = declarations(".state-screen");
    expect(screen).toMatch(/display:\s*flex\s*;/);
    expect(screen).toMatch(/flex-direction:\s*column\s*;/);
    expect(screen).toMatch(/align-items:\s*center\s*;/);
    expect(screen).toMatch(/text-align:\s*center\s*;/);
    expect(screen).toMatch(/height:\s*100%\s*;/);
  });

  it("centers the column vertically, and scrolls from the top once it is taller than the window", () => {
    expect(declarations(".state-screen")).toMatch(/overflow:\s*auto\s*;/);
    expect(declarations(".state-screen::before,\n.state-screen::after")).toMatch(/flex:\s*1\s+0\s+0\s*;/);
  });

  it("leaves the top of the window to the traffic lights, and lets it move the window", () => {
    expect(declarations(".state-drag")).toMatch(/position:\s*fixed\s*;/);
    expect(declarations(".state-screen")).toMatch(/padding:\s*52px\b/);
  });
});
