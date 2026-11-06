import { browser, expect } from "@wdio/globals";

/** The label of the one window, as `tauri.conf.json` names it. */
const MAIN = "main";

describe("the close button of the window", () => {
  it("hides the window and leaves the app running in the menu bar", async () => {
    await browser.waitUntil(
      async () => browser.execute(() => document.querySelector("h1")?.textContent === "Procly"),
      { timeout: 60_000, timeoutMsg: "the window did not draw" },
    );
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
