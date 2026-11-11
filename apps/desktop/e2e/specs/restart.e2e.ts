import { browser, expect } from "@wdio/globals";

import { procly, reportedService, stopDaemon, writeScene, type Scene } from "../environment";
import { openingWorktree, picking, pressNamed, showingButton } from "../window";

describe("a Service restarted from the Worktree screen", () => {
  let scene: Scene;

  before(async () => {
    scene = await writeScene("web-restart", "web");
  });

  after(async () => {
    await stopDaemon();
  });

  it("starts again with a new started_at, on the same port", async () => {
    const up = await procly(["up", "--json", scene.service], scene.path);
    expect(up.code).toBe(0);
    const before = await reportedService(scene, scene.service);
    expect(before?.state).toBe("healthy");
    expect(before?.port).not.toBeNull();

    // The Worktree is found by its folder. Every scene is on `main`, and until its next answer the window may still
    // list the Worktree of the flow before with a Service of the same name, whose Restart would reach nothing.
    await openingWorktree(scene.folder);
    // The row picks the Service, and the inspector holds its actions.
    await picking("main button.item-select", scene.service);
    await showingButton(`Restart ${scene.service}`);
    expect(await pressNamed(`Restart ${scene.service}`)).toBe(true);

    await browser.waitUntil(
      async () => {
        const now = await reportedService(scene, scene.service);
        return now !== null && now.state === "healthy" && now.started_at > (before?.started_at ?? 0);
      },
      { timeout: 60_000, interval: 500, timeoutMsg: "the Service did not start again with a new started_at" },
    );
    const after = await reportedService(scene, scene.service);
    expect(after?.port).toBe(before?.port);
  });
});
