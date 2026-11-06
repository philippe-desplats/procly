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
  });
});
