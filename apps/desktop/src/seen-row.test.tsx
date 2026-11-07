import { cleanup, render } from "@testing-library/react";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { afterEach, describe, expect, it } from "vitest";

import { ItemRow } from "./ItemRow";
import { foreignWord, jobEndWord, jobWord, serviceEndWord, serviceWord, type StateWord } from "./state";
const css = readFileSync(resolve(import.meta.dirname, "styles.css"), "utf8");

type Theme = "light" | "dark";
type Rgb = [number, number, number];

/** The hex tokens of the first block (light) and of the dark block, which restates only what changes. */
function palette(): Record<Theme, Map<string, string>> {
  const tokens = (block: string): Map<string, string> =>
    new Map(
      [...block.matchAll(/(--[\w-]+):\s*(#[0-9a-f]{6});/gi)].map((match) => [
        match[1] ?? "",
        (match[2] ?? "").toLowerCase(),
      ]),
    );
  const [lightBlock = "", afterDark = ""] = css.split("@media (prefers-color-scheme: dark)");
  const light = tokens(lightBlock);
  const dark = new Map([...light, ...tokens(afterDark.slice(0, afterDark.indexOf("@supports")))]);
  return { light, dark };
}

function channels(hex: string): Rgb {
  return [1, 3, 5].map((at) => Number.parseInt(hex.slice(at, at + 2), 16)) as Rgb;
}

function luminance(rgb: Rgb): number {
  const [r = 0, g = 0, b = 0] = rgb.map((channel) => {
    const value = channel / 255;
    return value <= 0.03928 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

function contrast(a: Rgb, b: Rgb): number {
  const [lighter = 0, darker = 0] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return (lighter + 0.05) / (darker + 0.05);
}

/** `top` laid over `under` with the opacity `alpha`, as the browser blends them. */
function blend(top: Rgb, under: Rgb, alpha: number): Rgb {
  return top.map((channel, at) => channel * alpha + (under[at] ?? 0) * (1 - alpha)) as Rgb;
}

/**
 * Puts the stylesheet in the document with every token already replaced by its value in `theme`. jsdom resolves no
 * `var()` and knows no color scheme, so the theme is chosen here, and what it computes is what the browser draws.
 */
function install(theme: Theme): void {
  const colours = palette()[theme];
  const withoutThemes = css
    .replace(/:root\s*\{[^}]*\}/g, "")
    .replace(/@media \(prefers-color-scheme: dark\)\s*\{\s*\}/g, "");
  const style = document.createElement("style");
  style.textContent = withoutThemes.replace(
    /var\((--[\w-]+)\)/g,
    (whole, token: string) => colours.get(token) ?? whole,
  );
  document.head.append(style);
}

/** Every state word the rows draw: a Service, a Job, and a process Procly did not start, ended or not. */
const WORDS: StateWord[] = [
  serviceWord({ state: "starting" } as Parameters<typeof serviceWord>[0], false),
  serviceWord({ state: "healthy" } as Parameters<typeof serviceWord>[0], false),
  serviceWord({ state: "restarting" } as Parameters<typeof serviceWord>[0], false),
  serviceEndWord({ kind: "stopped" }, false),
  serviceEndWord({ kind: "exited", code: 1 }, true),
  jobWord({ state: "running" } as Parameters<typeof jobWord>[0], false),
  jobEndWord({ kind: "exited", code: 0 }, false),
  jobEndWord({ kind: "timed_out", timeout_ms: 1000 }, true),
  foreignWord(),
];

function row(state: StateWord, seen: boolean, selected = false): HTMLElement {
  const { container } = render(
    <ul>
      <ItemRow
        target={{ kind: "job", id: "ea79fa81" }}
        seen={seen}
        state={state}
        name="node"
        moment="19:31:22"
        ports={[3000]}
      />
    </ul>,
  );
  const item = container.querySelector("li");
  if (item === null) throw new Error("no row drawn");
  if (selected) item.setAttribute("data-selected", "true");
  return item;
}

function computed(element: Element, property: string): string {
  return getComputedStyle(element).getPropertyValue(property).trim();
}

/** The colour a text is drawn in, from `rgb(r, g, b)`. */
function colourOf(element: Element): Rgb {
  const match = /^rgb\((\d+), (\d+), (\d+)\)$/.exec(computed(element, "color"));
  expect(match, `the colour of .${element.className} is a plain colour`).not.toBeNull();
  return [Number(match?.[1]), Number(match?.[2]), Number(match?.[3])];
}

/** How opaque an element is drawn, with the opacity of everything that holds it. */
function opacityOf(element: Element): number {
  let alpha = 1;
  for (let at: Element | null = element; at !== null; at = at.parentElement) {
    const opacity = computed(at, "opacity");
    alpha *= opacity === "" ? 1 : Number.parseFloat(opacity);
  }
  return alpha;
}

describe("a Seen row", () => {
  afterEach(() => {
    cleanup();
    document.head.innerHTML = "";
  });

  it.each(["light", "dark"] as const)(
    "keeps the word of every state at 4.5:1 or more on the panel and on the picked row, in the %s theme",
    (theme) => {
      install(theme);
      const colours = palette()[theme];
      const panel = channels(colours.get("--panel") ?? "");
      const accent = channels(colours.get("--accent") ?? "");
      const picked = blend(accent, panel, theme === "light" ? 0.14 : 0.22);
      for (const state of WORDS) {
        cleanup();
        const word = row(state, true, true).querySelector(".word");
        expect(word, state.word).not.toBeNull();
        const ink = colourOf(word as Element);
        const alpha = opacityOf(word as Element);
        for (const [where, background] of [
          ["the panel", panel],
          ["the picked row", picked],
        ] as const) {
          const drawn = blend(ink, background, alpha);
          expect(contrast(drawn, background), `${state.word} on ${where}, ${theme}`).toBeGreaterThanOrEqual(
            4.5,
          );
        }
      }
    },
  );

  it.each(["light", "dark"] as const)(
    "still reads as set aside in the %s theme: its word is the muted grey, and what is not the state fades",
    (theme) => {
      install(theme);
      const muted = channels(palette()[theme].get("--muted") ?? "");
      for (const state of WORDS) {
        cleanup();
        const seen = row(state, true);
        expect(colourOf(seen.querySelector(".word") as Element), `${state.word} when Seen`).toEqual(muted);
        for (const part of [".item-name", ".item-where", ".item-end > .caption", ".item-ports"]) {
          const element = seen.querySelector(part);
          if (element !== null) {
            expect(opacityOf(element), `${part} when Seen`).toBeLessThan(1);
          }
        }
      }
    },
  );

  it("keeps the glyph of its state undimmed, so that its shape is read at the contrast it needs", () => {
    install("light");
    for (const state of WORDS) {
      cleanup();
      const seen = row(state, true);
      expect(opacityOf(seen.querySelector(".state-glyph") as Element), state.word).toBe(1);
    }
  });

  it("differs from the same row when it is not Seen, which keeps the colour of its tone and its full opacity", () => {
    install("light");
    const failure = serviceEndWord({ kind: "exited", code: 1 }, true);
    const unseen = row(failure, false);
    expect(colourOf(unseen.querySelector(".word") as Element)).toEqual(
      channels(palette().light.get("--fail") ?? ""),
    );
    expect(opacityOf(unseen.querySelector(".item-name") as Element)).toBe(1);
  });
});
