import { spawn, type ChildProcess } from "node:child_process";

import { browser, expect } from "@wdio/globals";

import { freePort, procly, stopDaemon, testServer, writeScene, type Scene } from "../environment";
import { pick, press, showingText } from "../window";

/** The inspector of the window. */
const INSPECTOR = 'aside[aria-label="Inspector"]';

/** A row of Needs you on the Overview, which picks its item for the inspector. */
const NEEDS_YOU_ROW = 'section[aria-label="Needs you"] li.item button.item-select';

describe("a Foreign process selected in its Worktree", () => {
  let scene: Scene;
  // A server started in the Worktree without Procly, so that the Daemon reports it as a Foreign process.
  let foreign: ChildProcess | null = null;

  before(async () => {
    scene = await writeScene("foreign", "web");
  });

  after(async () => {
    // Procly never stops a process it did not start, so the test stops the one it started itself.
    foreign?.kill();
    await stopDaemon();
  });

  it("shows in the inspector the command that stops it, and no Stop of Procly's", async () => {
    const up = await procly(["up", "--json", scene.service], scene.path);
    expect(up.code).toBe(0);
    const port = await freePort();
    foreign = spawn(testServer(), ["--port", String(port)], { cwd: scene.path, stdio: "ignore" });
    const pid = foreign.pid;
    expect(pid).toBeDefined();

    // The Overview pins the Foreign process in Needs you, with the port it listens on drawn as a pill. Each try asks
    // the poller for a tick, since the Daemon reports the process only once it listens.
    await browser.waitUntil(
      async () => {
        await press("Refresh");
        return pick(NEEDS_YOU_ROW, String(port));
      },
      {
        timeout: 60_000,
        interval: 500,
        timeoutMsg: `no Foreign process on port ${String(port)} in Needs you`,
      },
    );

    await showingText(INSPECTOR, ["started outside Procly", "Stop it yourself", `kill ${String(pid)}`]);
    const stops = await browser.execute(
      (css: string) =>
        Array.from(document.querySelectorAll(`${css} button`)).filter((button) =>
          (button.getAttribute("aria-label") ?? button.textContent).startsWith("Stop"),
        ).length,
      INSPECTOR,
    );
    expect(stops).toBe(0);
  });
});
