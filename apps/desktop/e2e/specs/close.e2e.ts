import { browser, expect } from "@wdio/globals";

import { NOT_RUNNING, showingTitle } from "../window";

/** The label of the one window, as `tauri.conf.json` names it. */
const MAIN = "main";

describe("the close button of the window", () => {
  it("hides the window and leaves the app running in the menu bar", async () => {
    // The suite starts with no Daemon, so the window draws the full-window state screen, which has no sidebar.
    await showingTitle(NOT_RUNNING);
    // A window that is not shown yet, under the load of other workers, is `hidden` for a moment: wait for it rather than
    // read the state once. A locked Mac session keeps it hidden, which no wait cures.
    await browser.waitUntil(
      async () => (await browser.execute(() => document.visibilityState)) === "visible",
      {
        timeout: 15_000,
        interval: 250,
        timeoutMsg:
          "the window is not visible before its close button is pressed (is the Mac session locked?)",
      },
    );

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
