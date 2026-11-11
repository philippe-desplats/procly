import { browser, expect } from "@wdio/globals";

import { procly, stopDaemon, writeScene, type Scene } from "../environment";
import { showingText, textOf } from "../window";

/** The line of the Daemon at the foot of the sidebar. */
const DAEMON_LINE = 'nav[aria-label="Worktrees"] .daemon-line';

describe("the line of the Daemon at the foot of the sidebar", () => {
  let scene: Scene;

  before(async () => {
    scene = await writeScene("footer", "web");
  });

  after(async () => {
    await stopDaemon();
  });

  it("names the release the Daemon runs and the absolute time of the last Status, with a quiet glyph", async () => {
    const up = await procly(["up", "--json", scene.service], scene.path);
    expect(up.code).toBe(0);
    // The release is the one of the embedded `procly`, which started the Daemon: "procly 0.5.0".
    const version = (await procly(["--version"])).stdout.trim().split(" ").at(-1) ?? "";
    expect(version).not.toBe("");

    await showingText(DAEMON_LINE, [`Daemon ${version}, updated at `]);

    // An absolute time of day, never an age that would drift between two ticks.
    expect(await textOf(DAEMON_LINE)).toMatch(/updated at \d{2}:\d{2}:\d{2}$/);
    const glyph = await browser.execute(
      (css: string) => document.querySelector(`${css} svg`)?.getAttribute("data-glyph") ?? null,
      DAEMON_LINE,
    );
    expect(glyph).toBe("healthy");

    // The whole line shows at the default width of the window: its words are not cut with an ellipsis.
    const cut = await browser.execute((css: string) => {
      const words = document.querySelector<HTMLElement>(`${css} .daemon-line-words`);
      return words === null ? null : words.scrollWidth - words.clientWidth;
    }, DAEMON_LINE);
    expect(cut).not.toBeNull();
    expect(cut).toBeLessThanOrEqual(0);
  });

  it("still shows the whole line, words and time, whatever the font: 15 percent wider than the default", async () => {
    await showingText(DAEMON_LINE, ["updated at "]);
    // The line needs 214 px of the 220 px it has with the system font of macOS, and a font of another platform is wider
    // (WebKitGTK overflowed by 10 px). A font 15 percent larger stands in for it on any machine. It is set on the element
    // by script, since the content security policy of the app refuses a style the page adds in its markup.
    const measure = await browser.execute((css: string) => {
      const line = document.querySelector<HTMLElement>(css);
      const words = document.querySelector<HTMLElement>(`${css} .daemon-line-words`);
      const time = document.querySelector<HTMLElement>(`${css} .daemon-line-time`);
      const side = document.querySelector<HTMLElement>('nav[aria-label="Worktrees"]');
      if (line === null || words === null || time === null || side === null) {
        return null;
      }
      line.style.setProperty("font-size", "12.65px", "important");
      const edge = side.getBoundingClientRect().right;
      const measured = {
        font: getComputedStyle(line).fontSize,
        wordsCut: words.scrollWidth - words.clientWidth,
        // The words and the time stay inside the sidebar, never pushed past its edge or cut off by the line.
        wordsOutside: Math.max(0, words.getBoundingClientRect().right - edge),
        timeOutside: Math.max(0, time.getBoundingClientRect().right - edge),
        timeCut: time.scrollWidth - time.clientWidth,
        lineOverflow: line.scrollWidth - line.clientWidth,
      };
      line.style.removeProperty("font-size");
      return measured;
    }, DAEMON_LINE);

    expect(measure).not.toBeNull();
    // The font really was wider, so that the checks below are not a no-op.
    expect(measure?.font).toBe("12.65px");
    expect(measure?.wordsCut).toBeLessThanOrEqual(0);
    expect(measure?.wordsOutside).toBe(0);
    expect(measure?.timeCut).toBeLessThanOrEqual(0);
    expect(measure?.timeOutside).toBe(0);
    expect(measure?.lineOverflow).toBeLessThanOrEqual(0);
  });
});
