import { readFileSync } from "node:fs";

import { browser } from "@wdio/globals";

/** The smallest size `tauri.conf.json` gives the window, which a system resize is bound to and WebDriver is not. */
function minimumSize(): { width: number; height: number } {
  const config = JSON.parse(
    readFileSync(new URL("../src-tauri/tauri.conf.json", import.meta.url), "utf8"),
  ) as {
    app: { windows: { minWidth: number; minHeight: number }[] };
  };
  const window = config.app.windows[0];
  return { width: window?.minWidth ?? 0, height: window?.minHeight ?? 0 };
}

/**
 * Sets the size of the window through WebDriver, which bypasses the `minWidth` and `minHeight` of the window: only a
 * system resize is bound to them. A layout spec measuring a window smaller than a user can reach would prove nothing,
 * so a size under the minimum is refused here rather than measured.
 */
export async function resizeWindow(width: number, height: number): Promise<void> {
  const minimum = minimumSize();
  if (width < minimum.width || height < minimum.height) {
    throw new Error(
      `a ${String(width)}x${String(height)} window cannot be reached: the minimum is ${String(minimum.width)}x${String(minimum.height)}`,
    );
  }
  await browser.setWindowSize(width, height);
}

// The helpers below read and press the page through `browser.execute` rather than through `$` and `click`. The
// embedded WebDriver server of macOS takes about 5 seconds for each new element lookup, which made one flow last 4
// minutes, while a script runs in a few milliseconds. The page is the one the developer sees, only the way to reach
// it differs.

/** The Overview of every Service, the center of the window once a Daemon answers. */
export const OVERVIEW = 'section[aria-label="Overview"]';

/** The title of the screen the window shows when no Daemon answers. */
export const NOT_RUNNING = "The Daemon is not running";

/** The text of the first element `selector` matches, or `null` when none does. */
export async function textOf(selector: string): Promise<string | null> {
  return browser.execute((css: string) => document.querySelector(css)?.textContent ?? null, selector);
}

/** Whether an `h2` of the page says exactly `title`. */
export async function hasTitle(title: string): Promise<boolean> {
  return browser.execute(
    (wanted: string) =>
      Array.from(document.querySelectorAll("h2")).some((heading) => heading.textContent === wanted),
    title,
  );
}

/** Presses the enabled button labelled `label` and tells whether there was one. */
export async function press(label: string): Promise<boolean> {
  return browser.execute((wanted: string) => {
    const button = Array.from(document.querySelectorAll("button")).find(
      (candidate) => candidate.textContent.trim() === wanted && !candidate.disabled,
    );
    button?.click();
    return button !== undefined;
  }, label);
}

/**
 * Waits until `ready` holds. The poller ticks every 2 to 30 seconds depending on where the window stands, which a
 * window without a window manager does not report faithfully, so each try presses Refresh to ask for a tick now
 * rather than depend on the cadence. Refresh asks the poller and nothing else, it starts nothing in the Daemon. The
 * state screens of a Daemon that cannot be read have no Refresh, so there the own tick of the poller answers.
 */
async function until(what: string, ready: () => Promise<boolean>, within: number): Promise<void> {
  await browser.waitUntil(
    async () => {
      await press("Refresh");
      return ready();
    },
    { timeout: within, interval: 500, timeoutMsg: `${what} did not appear within ${within} ms` },
  );
}

/** Waits until the window shows the screen titled `title`. */
export async function showingTitle(title: string, within = 60_000): Promise<void> {
  await until(`the screen "${title}"`, () => hasTitle(title), within);
}

/** Waits until the text of `selector` holds every one of `parts`. */
export async function showingText(selector: string, parts: string[], within = 60_000): Promise<void> {
  await until(
    `${parts.join(", ")} in ${selector}`,
    async () => {
      const text = await textOf(selector);
      return text !== null && parts.every((part) => text.includes(part));
    },
    within,
  );
}

/** Whether the page has an enabled button whose accessible name is exactly `label`, such as `Restart web`. */
export async function hasButton(label: string): Promise<boolean> {
  return browser.execute(
    (wanted: string) =>
      Array.from(document.querySelectorAll("button")).some(
        (candidate) => candidate.getAttribute("aria-label") === wanted && !candidate.disabled,
      ),
    label,
  );
}

/** Presses the enabled button whose accessible name is exactly `label`, and tells whether there was one. */
export async function pressNamed(label: string): Promise<boolean> {
  return browser.execute((wanted: string) => {
    const button = Array.from(document.querySelectorAll("button")).find(
      (candidate) => candidate.getAttribute("aria-label") === wanted && !candidate.disabled,
    );
    button?.click();
    return button !== undefined;
  }, label);
}

/**
 * Opens the Worktree of the sidebar whose folder is `folder`, as its tooltip names it, and tells whether the sidebar
 * had it. The folder, not the name the row shows: every scene is on a branch `main`, and the window may still draw the
 * Worktrees of the Daemon a previous flow stopped until its next answer, so only the folder tells the scene's apart.
 */
export async function openWorktree(folder: string): Promise<boolean> {
  return browser.execute((wanted: string) => {
    const row = Array.from(document.querySelectorAll<HTMLElement>('nav[aria-label="Worktrees"] .row')).find(
      (candidate) => (candidate.getAttribute("title") ?? "").split("\n").includes(`Folder: ${wanted}`),
    );
    row?.click();
    return row !== undefined;
  }, folder);
}

/** Waits until the sidebar lists the Worktree whose folder is `folder`, then opens it. */
export async function openingWorktree(folder: string, within = 60_000): Promise<void> {
  await until(`the Worktree ${folder} in the sidebar`, () => openWorktree(folder), within);
}

/** Waits until the window has an enabled button named `label`. */
export async function showingButton(label: string, within = 60_000): Promise<void> {
  await until(`the button "${label}"`, () => hasButton(label), within);
}

/** The output pane of the inspector. */
export const OUTPUT = 'pre[aria-label="Output"]';

/** Presses the first element `selector` matches whose text holds `text`, and tells whether there was one. */
export async function pick(selector: string, text = ""): Promise<boolean> {
  return browser.execute(
    (css: string, wanted: string) => {
      const target = Array.from(document.querySelectorAll<HTMLElement>(css)).find((candidate) =>
        (candidate.textContent ?? "").includes(wanted),
      );
      target?.click();
      return target !== undefined;
    },
    selector,
    text,
  );
}

/** Waits until an element `selector` matches holds `text`, then presses the first such element. */
export async function picking(selector: string, text: string, within = 60_000): Promise<void> {
  await until(`${text} in ${selector}`, () => pick(selector, text), within);
}
