// @vitest-environment node
import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

const css = readFileSync(new URL("./styles.css", import.meta.url), "utf8").replace(/\/\*[\s\S]*?\*\//g, "");

/** The declarations of the first rule whose selector list holds `selector` as one of its members. */
function declarations(selector: string): string {
  const rules = [...css.matchAll(/(?:^|\})\s*([^{}@]+)\{([^}]*)\}/gm)];
  const found = rules.find((rule) => (rule[1] ?? "").split(",").some((member) => member.trim() === selector));
  return found?.[2] ?? "";
}

describe("the sheets of a screen that holds the whole window", () => {
  it("align their text on the left, though the column that holds them centers its own", () => {
    expect(declarations(".state-column")).toMatch(/text-align:\s*center\s*;/);
    expect(declarations(".state-column .sheet")).toMatch(/text-align:\s*left\s*;/);
  });
});

describe("Release… under the title line of a Worktree", () => {
  // The line sits at the top of the pane, so a margin of 18px leaves the 18px of air of the mockup.
  it("sits 18px under the line", () => {
    expect(declarations(".actions-row.release-row")).toMatch(/margin-top:\s*18px\s*;/);
  });
});

describe("the title line of a screen", () => {
  // The line is pinned by the 16px of padding of the pane it bleeds into, so it sticks 16px above where 0 would put it:
  // with 0 it sat 16px under its place in the flow, and the first row under it touched its rule (measured in the window).
  it("sticks at the place it takes in the flow, so that the content starts under its rule", () => {
    expect(declarations(".title-bar")).toMatch(/top:\s*-16px\s*;/);
    expect(declarations(".pane")).toMatch(/padding:\s*16px\s*;/);
  });
});

describe("the effects of a sheet", () => {
  it("are one bulleted list, indented as the mockup indents it", () => {
    expect(declarations(".bullets")).toMatch(/padding-left:\s*18px\s*;/);
  });
});

describe("the pid of a Foreign process", () => {
  // Measured in the window: beside a long name the detail was cut to "p", so the pid keeps its width and the name gives way.
  it("is read whole, never cut", () => {
    const rule = declarations(".item-main .caption-whole");
    expect(rule).toMatch(/flex:\s*none\s*;/);
    expect(rule).toMatch(/white-space:\s*nowrap\s*;/);
  });
});

describe("an Orphan row", () => {
  it("is one line of 3 columns: its glyph, what it is, and what deleting it does", () => {
    const row = declarations(".item-orphan");
    expect(row).toMatch(/display:\s*grid\s*;/);
    expect(row).toMatch(/grid-template-columns:\s*14px minmax\(0, 1fr\) minmax\(0, 1\.5fr\)\s*;/);
  });
});

describe("a port pill", () => {
  it("answers the pointer and the keyboard, being a button", () => {
    expect(declarations(".port:hover")).toMatch(/border-color:\s*var\(--faint\)\s*;/);
    expect(declarations(".port:focus-visible")).toMatch(/outline:\s*2px solid var\(--accent\)\s*;/);
  });
});
