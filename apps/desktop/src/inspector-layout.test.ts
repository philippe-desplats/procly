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

describe("the width of the inspector", () => {
  // jsdom loads no stylesheet, so the width is read from the CSS text.
  it("is drawn between 320 and 400 px, and the center takes the rest", () => {
    const columns = /grid-template-columns:\s*([^;]*);/.exec(declarations(".window"))?.[1] ?? "";

    expect(columns.replace(/\s+/g, " ")).toBe("220px minmax(0, 1fr) minmax(320px, 400px)");
  });

  it("leaves Settings its 2 panes, with no inspector to size", () => {
    const columns = /grid-template-columns:\s*([^;]*);/.exec(declarations(".window-settings"))?.[1] ?? "";

    expect(columns.replace(/\s+/g, " ")).toBe("220px minmax(0, 1fr)");
  });
});

describe("the inspector of a Foreign process", () => {
  it("draws a separator under its title block", () => {
    expect(declarations(".inspector-head")).toMatch(/border-bottom:\s*1px solid var\(--line\)\s*;/);
  });

  it("keeps the Process value on one line, cut with an ellipsis when it does not fit", () => {
    const line = declarations(".facts .facts-line");

    expect(line).toMatch(/white-space:\s*nowrap\s*;/);
    expect(line).toMatch(/overflow:\s*hidden\s*;/);
    expect(line).toMatch(/text-overflow:\s*ellipsis\s*;/);
    expect(line).toMatch(/min-width:\s*0\s*;/);
  });
});

describe("the output of a Service", () => {
  it("keeps its lines whole and scrolls sideways, until Wrap folds them", () => {
    expect(declarations(".output")).toMatch(/white-space:\s*pre\s*;/);
    expect(declarations(".output.wrap")).toMatch(/white-space:\s*pre-wrap\s*;/);
    expect(declarations(".output.wrap")).toMatch(/overflow-wrap:\s*anywhere\s*;/);
  });
});
