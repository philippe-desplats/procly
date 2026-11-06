import { expect } from "@wdio/globals";

import { procly, stopDaemon } from "../environment";
import { NOT_RUNNING, OVERVIEW, press, showingText, showingTitle } from "../window";

describe("the Daemon seen from the window", () => {
  after(async () => {
    await stopDaemon();
  });

  it("is not started by the app, and starts when the developer asks, through the embedded procly", async () => {
    await showingTitle(NOT_RUNNING);

    expect(await press("Start the Daemon")).toBe(true);

    await showingText(OVERVIEW, ["Overview"]);
  });

  it("falls back to the not-running screen when the CLI stops the Daemon, then starts it again", async () => {
    const stopped = await procly(["daemon", "stop", "--json"]);
    expect(stopped.code).toBe(0);
    expect(JSON.parse(stopped.stdout)).toMatchObject({ outcome: "stopped" });

    await showingTitle(NOT_RUNNING);
    expect(await press("Start the Daemon")).toBe(true);

    await showingText(OVERVIEW, ["Overview"]);
  });
});
