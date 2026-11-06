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
  // jsdom loads no stylesheet, so how the screen is laid out is read from the CSS text.
  it("fills the window and scrolls, with the top of the column under the traffic lights", () => {
    const screen = declarations(".state-screen");
    expect(screen).toMatch(/height:\s*100%\s*;/);
    expect(screen).toMatch(/overflow:\s*auto\s*;/);
    expect(Number(/padding:\s*(\d+)px\b/.exec(screen)?.[1])).toBeGreaterThanOrEqual(52);
  });

  it("puts a centered column of 480 px at the top, not in the middle of the window", () => {
    const column = declarations(".state-column");
    expect(column).toMatch(/max-width:\s*480px\s*;/);
    expect(column).toMatch(/margin:\s*0 auto\s*;/);
    expect(column).toMatch(/display:\s*flex\s*;/);
    expect(column).toMatch(/flex-direction:\s*column\s*;/);
    expect(column).toMatch(/align-items:\s*center\s*;/);
    expect(column).toMatch(/text-align:\s*center\s*;/);
    expect(css).not.toMatch(/\.state-screen::(before|after)/);
  });

  it("gives the checks the 620 px of their own block, and no rule narrows it again", () => {
    expect(declarations(".state-screen > section")).toMatch(/max-width:\s*620px\s*;/);
    const narrowing = [...css.matchAll(/(\.state-[a-z-]+[^{]*)\{([^}]*max-width:\s*480px[^}]*)\}/g)].map(
      (match) => match[1]?.trim(),
    );
    expect(narrowing).toEqual([".state-column"]);
  });

  it("draws the brand mark at the 22 px of the mockup", () => {
    const mark = declarations(".state-column > .brand-mark");
    expect(mark).toMatch(/width:\s*22px\s*;/);
    expect(mark).toMatch(/height:\s*22px\s*;/);
  });

  it("leaves the top of the window to the traffic lights, and lets it move the window", () => {
    expect(declarations(".state-drag")).toMatch(/position:\s*fixed\s*;/);
  });
});
