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

  it("keeps the name and its word on one line, never wrapping them", () => {
    const main = declarations(".item-main");
    expect(main).toMatch(/display:\s*flex\s*;/);
    expect(main).toMatch(/min-width:\s*0\s*;/);
    expect(main).toMatch(/overflow:\s*hidden\s*;/);
    expect(main).toMatch(/white-space:\s*nowrap\s*;/);
  });

  it("cuts a long name, a Job command, with an ellipsis and always shows the word of the state whole", () => {
    // The name gives way, and only it: it may shrink below its content, and it does not grow past it, so the word
    // follows it rather than sitting at the far end.
    const name = declarations(".item-main .item-name");
    expect(name).toMatch(/flex:\s*0 1 auto\s*;/);
    expect(declarations(".item-name")).toMatch(/min-width:\s*0\s*;/);
    expect(declarations(".item-name")).toMatch(/text-overflow:\s*ellipsis\s*;/);
    // The word and the detail never shrink.
    expect(declarations(".item-main .word")).toMatch(/flex:\s*none\s*;/);
    expect(declarations(".item-main .caption")).toMatch(/flex:\s*none\s*;/);
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
    expect(bar).toMatch(/min-height:\s*52px\s*;/);
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
