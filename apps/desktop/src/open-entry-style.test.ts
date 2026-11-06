// @vitest-environment node
import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

const css = readFileSync(new URL("./styles.css", import.meta.url), "utf8");

/** Every rule of the stylesheet outside an at-rule, as its selectors and its declarations. */
function rules(): { selectors: string[]; body: string }[] {
  const bare = css.replace(/\/\*[\s\S]*?\*\//g, "");
  return [...bare.matchAll(/([^{}@]+)\{([^{}]*)\}/g)].map((match) => ({
    selectors: (match[1] ?? "").split(",").map((selector) => selector.trim()),
    body: match[2] ?? "",
  }));
}

/** The rules that style the open entry of the sidebar, the one carrying `aria-current`. */
function openEntryRules(): { selectors: string[]; body: string }[] {
  return rules().filter((rule) =>
    rule.selectors.some((selector) => selector.includes('[aria-current="true"]')),
  );
}

describe("the open entry of the sidebar", () => {
  // jsdom loads no stylesheet, so the look of the entry is read from the CSS text.
  it("is tinted with the selection tint of the lists, as the mockup draws it", () => {
    const entry = rules().find((rule) => rule.selectors.includes('.row[aria-current="true"]'));
    expect(entry?.body).toMatch(/background:\s*var\(--selected\)\s*;/);
  });

  it("is never filled with the solid accent", () => {
    for (const rule of openEntryRules()) {
      expect(rule.body, rule.selectors.join(", ")).not.toMatch(/background:\s*var\(--accent\)/);
    }
  });

  it("recolours no glyph and no text but the caption, so that the cross of a failure keeps its cut", () => {
    // A white or inherited colour turned the disc of a failure into the blank disc of a healthy Service.
    for (const rule of openEntryRules()) {
      const label = rule.selectors.join(", ");
      expect(rule.body, label).not.toMatch(/(?:fill|stroke)\s*:/);
      expect(rule.body, label).not.toMatch(/color\s*:\s*(?:inherit|#|rgb|white)/);
    }
    expect(
      openEntryRules()
        .flatMap((rule) => rule.selectors)
        .join(" "),
    ).not.toMatch(/glyph|state|reason|count/);
  });

  it("draws a caption in the muted colour, which stays readable on the tint", () => {
    const caption = rules().find((rule) => rule.selectors.includes('.row[aria-current="true"] .caption'));
    expect(caption?.body).toMatch(/color:\s*var\(--muted\)\s*;/);
  });
});
