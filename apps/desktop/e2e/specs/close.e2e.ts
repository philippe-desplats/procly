import { browser, expect } from "@wdio/globals";

import { NOT_RUNNING, showingTitle } from "../window";

/** The label of the one window, as `tauri.conf.json` names it. */
const MAIN = "main";

describe("the close button of the window", () => {
  it("hides the window and leaves the app running in the menu bar", async () => {
    // Every flow before stopped its Daemon, so the window draws the full-window state screen, which has no sidebar. The
    // flow runs last (`wdio.conf.ts`), since the window it hides stays hidden for the rest of the run.
    await showingTitle(NOT_RUNNING);
    expect(await browser.execute(() => document.visibilityState)).toBe("visible");

    // Close Window asks the window to close as its close button does, through the CloseRequested handler of the app,
    // and answers the windows left once the close is done or stopped.
    const left = await browser.closeWindow();

    expect(left).toContain(MAIN);
    await browser.waitUntil(
      async () => (await browser.execute(() => document.visibilityState)) === "hidden",
      {
        timeout: 10_000,
        timeoutMsg: "the window is still shown after its close button",
      },
    );
    // The app did not quit: it still answers, with its window kept.
    expect(await browser.getWindowHandles()).toContain(MAIN);
  });
});
