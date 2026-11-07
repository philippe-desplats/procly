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

/** The smallest width the window can be given, as `tauri.conf.json` fixes it. */
function minimumWindowWidth(): number {
  const config = JSON.parse(
    readFileSync(new URL("../src-tauri/tauri.conf.json", import.meta.url), "utf8"),
  ) as { app: { windows: { minWidth: number }[] } };
  return config.app.windows[0]?.minWidth ?? 0;
}

describe("the width of the inspector", () => {
  // jsdom loads no stylesheet, so the width is read from the CSS text.
  it("is drawn between 320 and 400 px, and the center keeps at least 260 px", () => {
    const columns = /grid-template-columns:\s*([^;]*);/.exec(declarations(".window"))?.[1] ?? "";

    expect(columns.replace(/\s+/g, " ")).toBe("220px minmax(260px, 1fr) minmax(320px, 400px)");
  });

  it("gives way before the center does: at the smallest window the 3 minimums fit, so the inspector sits at 320 px", () => {
    const columns = /grid-template-columns:\s*([^;]*);/.exec(declarations(".window"))?.[1] ?? "";
    const widths = [...columns.matchAll(/(\d+)px/g)].map((match) => Number(match[1]));
    // The sidebar, the minimum of the center, the minimum of the inspector, then its maximum.
    const [sidebar = 0, center = 0, inspector = 0] = widths;

    expect(sidebar + center + inspector).toBeLessThanOrEqual(minimumWindowWidth());
    // The grid hands the free space to the inspector first, up to its maximum, so the center only keeps its minimum
    // when the window is too narrow for the inspector to reach it.
    expect(center).toBeGreaterThanOrEqual(260);
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

describe("the room of the output in the inspector", () => {
  // The heights are proved on the computed style of the real inspector in `inspector-bounds.test.tsx`, this reads
  // the declarations that test relies on.
  it("fills what is left under the facts, never less than 120 px, and the inspector scrolls when it does not fit", () => {
    expect(declarations(".inspector-panel")).toMatch(/display:\s*flex\s*;/);
    expect(declarations(".inspector-panel")).toMatch(/flex-direction:\s*column\s*;/);
    expect(declarations(".inspector-panel")).toMatch(/flex:\s*1 0 auto\s*;/);
    expect(declarations(".output-pane")).toMatch(/flex:\s*1 1 auto\s*;/);
    expect(declarations(".output-box")).toMatch(/flex:\s*1 1 0px\s*;/);
    expect(declarations(".output-box")).toMatch(/min-height:\s*120px\s*;/);
    expect(declarations(".output")).toMatch(/position:\s*absolute\s*;/);
    expect(declarations(".output")).not.toMatch(/max-height/);
    expect(declarations(".inspector")).toMatch(/display:\s*flex\s*;/);
    expect(declarations(".inspector")).toMatch(/flex-direction:\s*column\s*;/);
  });
});

describe("the output of a Service", () => {
  it("keeps its lines whole and scrolls sideways, until Wrap folds them", () => {
    expect(declarations(".output")).toMatch(/white-space:\s*pre\s*;/);
    expect(declarations(".output.wrap")).toMatch(/white-space:\s*pre-wrap\s*;/);
    expect(declarations(".output.wrap")).toMatch(/overflow-wrap:\s*anywhere\s*;/);
  });
});
