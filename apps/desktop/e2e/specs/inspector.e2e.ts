import { writeFileSync } from "node:fs";
import { join } from "node:path";

import { browser, expect } from "@wdio/globals";

import { procly, stopDaemon, writeScene, type Scene } from "../environment";
import { FIRST_WORKTREE, OUTPUT, pick, showingText } from "../window";

/** The Job waits for a file to exist before it prints its second line, so the test decides when it is written. */
const COMMAND = "sh -c 'echo before; while [ ! -f go ]; do sleep 0.2; done; echo after-open; sleep 120'";

describe("the output of a Job followed in the inspector", () => {
  let scene: Scene;

  before(async () => {
    scene = await writeScene("job-output", "web");
  });

  after(async () => {
    await stopDaemon();
  });

  it("shows a line written after the logs were opened, which arrives through the Channel", async () => {
    const started = await procly(["run", COMMAND], scene.path);
    expect(started.code).toBe(0);

    // The sidebar lists the Worktree once the Daemon reports the Job, then the Job is picked in the center.
    await browser.waitUntil(async () => pick(FIRST_WORKTREE), { timeout: 60_000, interval: 500 });
    await browser.waitUntil(async () => pick("li.item button.item-select", "while [ ! -f go ]"), {
      timeout: 60_000,
      interval: 500,
    });

    await showingText(OUTPUT, ["before"]);
    // Nothing has printed the second line yet, it is written only now that the window follows the Job.
    writeFileSync(join(scene.path, "go"), "");

    await showingText(OUTPUT, ["before", "after-open"]);
  });
});
