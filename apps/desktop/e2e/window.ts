import { browser } from "@wdio/globals";

// The helpers below read and press the page through `browser.execute` rather than through `$` and `click`. The
// embedded WebDriver server of macOS takes about 5 seconds for each new element lookup, which made one flow last 4
// minutes, while a script runs in a few milliseconds. The page is the one the developer sees, only the way to reach
// it differs.

/** The Overview of every Service, the center of the window once a Daemon answers. */
export const OVERVIEW = 'section[aria-label="Overview"]';

/** The title of the screen the window shows when no Daemon answers. */
export const NOT_RUNNING = "Daemon not running";

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
 * rather than depend on the cadence. Refresh asks the poller and nothing else, it starts nothing in the Daemon.
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

/** The first worktree of the sidebar, once a Daemon reports one. A scene has one, so it is the scene's. */
export const FIRST_WORKTREE = 'nav[aria-label="Worktrees"] section.group ul.list li button';

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
