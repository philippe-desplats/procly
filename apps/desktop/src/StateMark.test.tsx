import { render } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import type { Glyph, StateWord } from "./state";
import { State } from "./StateMark";

/** The 6 states of the ticket, each as the word and tone it is drawn with in the app. */
const SIX: Record<string, StateWord> = {
  healthy: { word: "healthy", tone: "ok", glyph: "healthy" },
  ended: { word: "stopped", tone: "ended", glyph: "ended" },
  restarting: { word: "restarting", tone: "warn", glyph: "busy" },
  failure: { word: "exited 1", tone: "fail", glyph: "failure" },
  job: { word: "running", tone: "muted", glyph: "job" },
  foreign: { word: "started outside Procly", tone: "warn", glyph: "foreign" },
};

function glyphOf(state: StateWord): SVGSVGElement {
  const { container } = render(<State {...state} />);
  const svg = container.querySelector("svg");
  if (svg === null) throw new Error("no glyph drawn");
  return svg;
}

describe("the state glyph", () => {
  it("draws a different shape for each of the 6 states, whatever the colour", () => {
    // The colour comes from the stylesheet, so the markup of a glyph is its shape and nothing else.
    const shapes = Object.values(SIX).map((state) => glyphOf(state).innerHTML);

    expect(new Set(shapes).size).toBe(6);
  });

  it("marks each glyph with the name of its shape", () => {
    for (const state of Object.values(SIX)) {
      expect(glyphOf(state).getAttribute("data-glyph")).toBe(state.glyph);
    }
  });

  it("draws the checks of the Doctor with a tick, a dash and a triangle, different from the 6 states", () => {
    const checks: Glyph[] = ["passed", "skipped", "warning"];
    const shapes = [...Object.values(SIX).map((state) => state.glyph), ...checks].map(
      (glyph) => glyphOf({ word: glyph, tone: "muted", glyph }).innerHTML,
    );

    expect(new Set(shapes).size).toBe(9);
  });

  it("spins only the restarting ring", () => {
    const spinning = Object.entries(SIX)
      .filter(([, state]) => glyphOf(state).querySelector(".spin") !== null)
      .map(([name]) => name);

    expect(spinning).toEqual(["restarting"]);
  });

  it("is decorative, the word next to it stays in the text", () => {
    const { container } = render(<State {...SIX.failure!} />);

    expect(container.querySelector("svg")).toHaveAttribute("aria-hidden", "true");
    expect(container.querySelector(".state")).toHaveTextContent("exited 1");
  });

  it("keeps the tone class on the state, which the stylesheet colours the glyph and the word with", () => {
    const { container } = render(<State {...SIX.failure!} />);

    expect(container.querySelector(".state")).toHaveClass("tone-fail");
  });
});
