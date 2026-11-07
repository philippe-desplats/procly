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

describe("a row of the center", () => {
  // jsdom loads no stylesheet, so the layout of a row is read from the CSS text.
  it("starts with a column for the glyph, then the name, then the time and the ports", () => {
    expect(declarations(".item-select")).toMatch(/display:\s*grid\s*;/);
    expect(declarations(".item-select")).toMatch(/grid-template-columns:\s*14px minmax\(0, 1fr\) auto\s*;/);
  });

  it("has a column for where it runs when it says so, between the name and the ports", () => {
    expect(declarations(".item-select-wide")).toMatch(
      /grid-template-columns:\s*14px minmax\(0, 1\.3fr\) minmax\(0, 1fr\) auto\s*;/,
    );
  });

  it("cuts the name and its word with an ellipsis rather than wrapping them", () => {
    const main = declarations(".item-main");
    expect(main).toMatch(/min-width:\s*0\s*;/);
    expect(main).toMatch(/overflow:\s*hidden\s*;/);
    expect(main).toMatch(/text-overflow:\s*ellipsis\s*;/);
    expect(main).toMatch(/white-space:\s*nowrap\s*;/);
  });

  it("gives the ports a slot of their own, so that the pills of a list end on the same edge", () => {
    expect(declarations(".item-ports")).toMatch(/justify-content:\s*flex-end\s*;/);
    expect(declarations(".item-ports")).toMatch(/min-width:\s*\d+px\s*;/);
  });
});

describe("the title line of a screen", () => {
  it("is as tall as the strip over the sidebar and separated from the list by a line", () => {
    const bar = declarations(".title-bar");
    expect(bar).toMatch(/display:\s*flex\s*;/);
    expect(bar).toMatch(/height:\s*52px\s*;/);
    expect(bar).toMatch(/border-bottom:\s*1px solid var\(--line\)\s*;/);
  });
});

describe("the hint of an empty inspector", () => {
  it("sits in the middle of the pane, in the faint colour", () => {
    const hint = declarations(".hint");
    expect(hint).toMatch(/align-items:\s*center\s*;/);
    expect(hint).toMatch(/justify-content:\s*center\s*;/);
    expect(hint).toMatch(/color:\s*var\(--faint\)\s*;/);
  });
});
