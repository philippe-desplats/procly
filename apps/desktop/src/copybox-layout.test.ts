// @vitest-environment node
import { readdirSync, readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

const css = readFileSync(new URL("./styles.css", import.meta.url), "utf8");

/** The declarations of the first rule whose selector is exactly `selector`. */
function declarations(selector: string): string {
  const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const match = new RegExp(`(?:^|\\})\\s*${escaped}\\s*\\{([^}]*)\\}`, "m").exec(css);
  return match?.[1] ?? "";
}

/** The selectors of the rules that mention a copy box, whatever they set. */
function copyBoxSelectors(): string[] {
  return [...css.matchAll(/(?:^|\})\s*([^{}@/]*\.copybox[^{}]*)\{/gm)].map((match) =>
    (match[1] ?? "").trim(),
  );
}

describe("the copy box layout", () => {
  // jsdom loads no stylesheet, so the wrapping that keeps Copy inside a sheet is read from the CSS text.
  it("lets the command shrink and wrap, so a long path never pushes Copy out", () => {
    const code = declarations(".copybox code");
    expect(code).toMatch(/min-width:\s*0\s*;/);
    expect(code).toMatch(/flex:\s*1\s*;/);
    expect(code).toMatch(/white-space:\s*pre-wrap\s*;/);
    expect(code).toMatch(/overflow-wrap:\s*anywhere\s*;/);
  });

  it("keeps the Copy button at its natural size", () => {
    expect(declarations(".copybox-button")).toMatch(/flex:\s*none\s*;/);
  });
});

describe("the copy box style", () => {
  it("sets the box on the side background and not on the raised one", () => {
    const box = declarations(".copybox");
    expect(box).toMatch(/background:\s*var\(--side\)\s*;/);
    expect(box).not.toMatch(/--raise/);
  });

  it("draws the Copy button without a border, quiet, and raises it only under the pointer", () => {
    const button = declarations(".copybox-button");
    expect(button).toMatch(/border:\s*(?:0|none)\s*;/);
    expect(button).toMatch(/background:\s*transparent\s*;/);
    expect(button).toMatch(/color:\s*var\(--muted\)\s*;/);
    expect(button).toMatch(/font-size:\s*var\(--size-small\)\s*;/);
    const hover = declarations(".copybox-button:hover");
    expect(hover).toMatch(/background:\s*var\(--raise\)\s*;/);
    expect(hover).toMatch(/color:\s*var\(--text\)\s*;/);
  });

  it("leaves the Copy button a visible focus ring", () => {
    expect(declarations(".copybox-button:focus-visible")).toMatch(/outline:/);
  });

  it("restyles no copy box in a particular place, so that every copy box looks the same", () => {
    const allowed = [
      ".copybox",
      ".copybox code",
      ".copybox-button",
      ".copybox-button:hover",
      ".copybox-button:focus-visible",
    ];
    for (const selector of copyBoxSelectors()) {
      if (allowed.includes(selector)) {
        continue;
      }
      // A rule that places a box in its parent may do so, one that paints it may not.
      expect(declarations(selector), selector).not.toMatch(/background|border|color|font-size|padding/);
    }
  });

  it("draws every copy box through the CopyBox component, which is the only file that names the classes", () => {
    const sources = readdirSync(new URL("./", import.meta.url)).filter(
      (file) => /\.tsx?$/.test(file) && !/\.test\.tsx?$/.test(file),
    );
    const naming = sources.filter((file) =>
      /copybox/.test(readFileSync(new URL(`./${file}`, import.meta.url), "utf8")),
    );
    expect(naming).toEqual(["CopyBox.tsx"]);
  });
});
