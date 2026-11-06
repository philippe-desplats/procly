import { browser, expect } from "@wdio/globals";

import { procly, reportedService, stopDaemon, writeScene, type Scene } from "../environment";
import { openWorktree, pick, pressNamed, showingButton } from "../window";

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

    // The scene's branch is `main`, which is what the sidebar names the Worktree.
    await browser.waitUntil(() => openWorktree("main"), { timeout: 60_000, interval: 500 });
    // The row picks the Service, and the inspector holds its actions.
    await browser.waitUntil(() => pick("main button.item-select", scene.service), {
      timeout: 60_000,
      interval: 500,
    });
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
