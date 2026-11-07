import { browser, expect } from "@wdio/globals";

import { procly, reportedService, stopDaemon, writeScene, type Scene } from "../environment";
import { openWorktree, press, showingText, showingTitle } from "../window";

/** The sheet the Release button opens, and the one that shows the report. */
const SHEET = "div.sheet";

describe("a Worktree released through its sheet", () => {
  let scene: Scene;

  before(async () => {
    scene = await writeScene("web-release", "web");
  });

  after(async () => {
    await stopDaemon();
  });

  it("reads what the release would do first, changes nothing until confirmed, then stops the Service", async () => {
    const up = await procly(["up", "--json", scene.service], scene.path);
    expect(up.code).toBe(0);
    expect((await reportedService(scene, scene.service))?.state).toBe("healthy");

    // The scene's branch is `main`, which is what the sidebar names the Worktree.
    await browser.waitUntil(() => openWorktree("main"), { timeout: 60_000, interval: 500 });
    await browser.waitUntil(() => press("Release…"), { timeout: 60_000, interval: 500 });

    // The dry run names the Service it would stop, and the Daemon has not touched it.
    await showingTitle("Release main?");
    await showingText(SHEET, ["Will stop", scene.service]);
    expect((await reportedService(scene, scene.service))?.state).toBe("healthy");

    expect(await press("Release")).toBe(true);

    await showingTitle("Released main");
    await showingText(SHEET, ["Stopped", scene.service]);
    // The real release stopped the Service and forgot it.
    await browser.waitUntil(async () => (await reportedService(scene, scene.service)) === null, {
      timeout: 60_000,
      interval: 500,
      timeoutMsg: "the Service is still reported after the release",
    });
    expect(await press("Done")).toBe(true);
  });
});
