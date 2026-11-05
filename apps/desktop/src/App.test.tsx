import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { emit } from "@tauri-apps/api/event";
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { afterEach, describe, expect, it } from "vitest";

import { App } from "./App";
import { createDaemonStore, type Failure, type Update } from "./daemon";
import recorded from "./fixtures/status.json";
import overview from "./fixtures/status-overview.json";
import type { Response } from "./protocol/Response";

/** A `Status { all: true }` answer recorded from a running Daemon, its paths replaced by neutral ones. */
const status: Response = recorded as Response;

/** An answer with 3 Repositories, 6 Worktrees and every kind of label, in the shape of the recorded one. */
const many: Response = overview as Response;

/** When the poller read the answer. */
const AT = Date.UTC(2026, 10, 5, 19, 35, 0);

function answered(response: Response, at = AT): Update {
  return { at_ms: at, response };
}

function failed(kind: Exclude<Failure["kind"], "protocol_mismatch">, message: string, at = AT): Update {
  return { at_ms: at, failure: { kind, message } };
}

/** Plays the Rust side: `last` is what the poller has already answered, and every command the window sends is kept. */
function poller(last: Update | null = null): string[] {
  const commands: string[] = [];
  mockIPC(
    (command) => {
      commands.push(command);
      if (command === "last_update") {
        return last;
      }
      return command === "refresh" ? undefined : Promise.reject(new Error(`unexpected command ${command}`));
    },
    { shouldMockEvents: true },
  );
  return commands;
}

/** What the poller sends the window on its next tick. */
async function tick(update: Update): Promise<void> {
  await act(async () => {
    await emit("daemon-update", update);
  });
}

describe("the window", () => {
  afterEach(() => {
    cleanup();
    clearMocks();
  });

  it("draws the 3 panes and the Worktrees of the answer the poller already has", async () => {
    const commands = poller(answered(status));

    render(<App store={createDaemonStore()} />);

    const sidebar = screen.getByRole("navigation", { name: "Worktrees" });
    expect(await within(sidebar).findByRole("button", { name: /^main\s*main$/ })).toBeInTheDocument();
    expect(within(sidebar).getByRole("button", { name: "web-fix" })).toBeInTheDocument();
    expect(within(sidebar).getByRole("heading", { name: "web" })).toBeInTheDocument();
    expect(screen.getByRole("main")).toBeInTheDocument();
    expect(screen.getByRole("complementary", { name: "Inspector" })).toBeInTheDocument();
    expect(commands).toContain("last_update");
  });

  it("never asks the Daemon for anything itself, the poller does", async () => {
    const commands = poller(answered(status));

    render(<App store={createDaemonStore()} />);
    await screen.findByRole("button", { name: "web-fix" });

    expect(commands).toEqual(["last_update"]);
  });

  it("follows the poller: a new answer replaces the one on screen without a request", async () => {
    poller(null);
    render(<App store={createDaemonStore()} />);
    expect(await screen.findAllByText("Reading the Daemon…")).not.toHaveLength(0);

    await tick(answered(status));
    expect(await screen.findByRole("button", { name: "web-fix" })).toBeInTheDocument();

    await tick(answered(many));
    expect(await screen.findByRole("button", { name: /^docs-old/ })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "web-fix" })).not.toBeInTheDocument();
  });

  it("opens a Worktree from the sidebar", async () => {
    poller(answered(status));
    render(<App store={createDaemonStore()} />);

    fireEvent.click(await screen.findByRole("button", { name: "web-fix" }));

    const center = screen.getByRole("main");
    expect(await within(center).findByText("exited 3")).toBeInTheDocument();
    expect(screen.getByText("/p/work/acme/web-fix")).toBeInTheDocument();
  });

  it("says that no Daemon runs, and sends nothing but the request for the last answer", async () => {
    const commands = poller(failed("unreachable", "the daemon did not answer on /p/state/procly.sock"));

    render(<App store={createDaemonStore()} />);

    expect(await screen.findByRole("heading", { name: "Daemon not running" })).toBeInTheDocument();
    expect(screen.getByText(/does not start one/)).toBeInTheDocument();
    expect(commands).toEqual(["last_update"]);
  });

  it("shows why a Daemon that does not answer well failed", async () => {
    poller(failed("decode", "unreadable answer from the daemon"));

    render(<App store={createDaemonStore()} />);

    expect(await screen.findByText("unreadable answer from the daemon")).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "Daemon not running" })).not.toBeInTheDocument();
  });

  it("asks the poller for a tick when Refresh is pressed, and does not ask the Daemon", async () => {
    const commands = poller(answered(status));
    render(<App store={createDaemonStore()} />);

    fireEvent.click(await screen.findByRole("button", { name: "Refresh" }));

    await act(async () => {
      await Promise.resolve();
    });
    expect(commands).toEqual(["last_update", "refresh"]);
  });
});

describe("the Overview", () => {
  afterEach(() => {
    cleanup();
    clearMocks();
  });

  it("is what the window opens on, and lists every Service of every Worktree with its port", async () => {
    poller(answered(many));

    render(<App store={createDaemonStore()} />);

    const overviewPane = await screen.findByRole("region", { name: "Overview" });
    const rows = within(overviewPane).getAllByRole("listitem");
    expect(rows.map((row) => row.textContent)).toEqual([
      "apihealthy:1873119:31:22",
      "workerrestarting19:31:22",
      "apiexited 1:187422026-11-04 19:31:22",
      "sitestarting:1875519:31:40",
      "sitestopped:1876019:30:00",
    ]);
    // A Service without a port shows none instead of a placeholder.
    expect(rows[1]?.textContent).not.toMatch(/:\d{4,5}/);
  });

  it("groups the Services under the label of their Worktree", async () => {
    poller(answered(many));

    render(<App store={createDaemonStore()} />);

    const overviewPane = await screen.findByRole("region", { name: "Overview" });
    const headings = within(overviewPane)
      .getAllByRole("heading", { level: 3 })
      .map((heading) => heading.textContent);
    expect(headings).toEqual([
      "main",
      "feature/a-very-long-branch-name-that-the-sidebar-has-to-cut-at-its-end",
      "docs-preview",
      "docs-oldRemoved",
    ]);
  });

  it("shows when each Service started as an absolute time, never as an age, and when the answer was read", async () => {
    poller(answered(many));

    render(<App store={createDaemonStore()} />);

    const overviewPane = await screen.findByRole("region", { name: "Overview" });
    expect(within(overviewPane).queryByText(/\bago\b/)).not.toBeInTheDocument();
    expect(within(overviewPane).getByText("2026-11-04 19:31:22")).toBeInTheDocument();
    expect(screen.getByText("Updated 19:35:00")).toBeInTheDocument();
  });

  it("says so when no Service runs anywhere", async () => {
    poller(answered(status));
    render(<App store={createDaemonStore()} />);
    await screen.findByRole("region", { name: "Overview" });

    await tick(answered({ response: "status", repositories: [], jobs_outside_repositories: [] }));

    expect(await screen.findByText("No Service is known.")).toBeInTheDocument();
  });

  it("comes back with ⌘0 from a Worktree", async () => {
    poller(answered(many));
    render(<App store={createDaemonStore()} />);
    fireEvent.click(await screen.findByRole("button", { name: "docs-preview" }));
    expect(screen.queryByRole("region", { name: "Overview" })).not.toBeInTheDocument();

    fireEvent.keyDown(window, { key: "0", metaKey: true });

    expect(await screen.findByRole("region", { name: "Overview" })).toBeInTheDocument();
  });
});

describe("the sidebar", () => {
  afterEach(() => {
    cleanup();
    clearMocks();
  });

  it("lists the Worktrees by branch, by folder name when there is no branch, and marks the removed ones", async () => {
    poller(answered(many));

    render(<App store={createDaemonStore()} />);

    const sidebar = screen.getByRole("navigation", { name: "Worktrees" });
    await within(sidebar).findByRole("button", { name: "docs-preview" });
    const labels = within(sidebar)
      .getAllByRole("button")
      .map((button) => button.textContent)
      .filter((text) => text !== "Overview⌘0");
    expect(labels).toEqual([
      "mainmain",
      "feature/a-very-long-branch-name-that-the-sidebar-has-to-cut-at-its-end",
      "mainmain",
      "docs-preview",
      "docs-oldRemoved",
      "mainmain",
    ]);
    expect(within(sidebar).queryByText(/detached/i)).not.toBeInTheDocument();
    expect(within(sidebar).getAllByText("Removed")).toHaveLength(1);
  });

  it("puts the Overview first, open", async () => {
    poller(answered(many));

    render(<App store={createDaemonStore()} />);

    const sidebar = screen.getByRole("navigation", { name: "Worktrees" });
    const overviewButton = await within(sidebar).findByRole("button", { name: /^Overview/ });
    expect(overviewButton).toHaveAttribute("aria-current", "true");
    expect(within(sidebar).getAllByRole("button")[0]).toBe(overviewButton);
  });

  it("names the branch, the folder and the slug in the tooltip, and cuts a long name at its end", async () => {
    poller(answered(many));

    render(<App store={createDaemonStore()} />);

    const sidebar = screen.getByRole("navigation", { name: "Worktrees" });
    const long = await within(sidebar).findByRole("button", {
      name: "feature/a-very-long-branch-name-that-the-sidebar-has-to-cut-at-its-end",
    });
    expect(long).toHaveAttribute(
      "title",
      "Branch: feature/a-very-long-branch-name-that-the-sidebar-has-to-cut-at-its-end\nFolder: web-login\nSlug: web_login",
    );
    expect(within(long).getByText(/^feature\//)).toHaveClass("row-name");
    const detached = within(sidebar).getByRole("button", { name: "docs-preview" });
    expect(detached).toHaveAttribute("title", "Branch: none\nFolder: docs-preview\nSlug: docs_preview");
  });
});
