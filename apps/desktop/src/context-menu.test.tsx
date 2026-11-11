import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { emit } from "@tauri-apps/api/event";
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { afterEach, describe, expect, it } from "vitest";

import { App } from "./App";
import { CONTEXT_CHOICE } from "./context-menu";
import { createDaemonStore, type FailureKey, type NeedsYou, type Update } from "./daemon";
import type { ContextEntry } from "./protocol/ContextEntry";
import type { Response } from "./protocol/Response";

const AT = Date.UTC(2026, 10, 7, 21, 0, 0);
const PATH = "/p/work/acme/web-fix";

/** A Worktree with a Service listening on a port, a Job that failed and a Foreign process. */
const scene: Response = {
  response: "status",
  repositories: [
    {
      git_common_dir: "/p/work/acme/web/.git",
      worktrees: [
        {
          path: PATH,
          slug: "web_fix",
          main: false,
          branch: "web-fix",
          listed: true,
          services: [
            {
              service: "web",
              port: 18731,
              reattached: false,
              started_at: AT - 60_000,
              restartable: true,
              state: "healthy",
            },
          ],
          jobs: [
            {
              id: "a4e2ea77",
              command: "sh -c 'exit 3'",
              cwd: PATH,
              state: "ended",
              end: { kind: "exited", code: 3 },
            },
          ],
          foreign: [{ kind: "process", pid: 4812, command: "node server.js", ports: [3000] }],
          containers: [],
        },
      ],
    },
  ],
  jobs_outside_repositories: [],
};

const JOB_KEY: FailureKey = { kind: "job", id: "a4e2ea77" };

const failedJob: NeedsYou = {
  kind: "job",
  worktree: PATH,
  id: "a4e2ea77",
  command: "sh -c 'exit 3'",
  end: { kind: "exited", code: 3 },
};

const foreignLine: NeedsYou = {
  kind: "foreign",
  worktree: PATH,
  process: { kind: "process", pid: 4812, command: "node server.js", ports: [3000] },
};

/** The answer while the Job is not Seen, and while it is. */
const unseen: Update = { at_ms: AT, response: scene, needs_you: [failedJob, foreignLine], seen: [] };
const seen: Update = { at_ms: AT, response: scene, needs_you: [foreignLine], seen: [JOB_KEY] };

interface Call {
  command: string;
  args: unknown;
}

/**
 * Plays the Rust side from the answer `last`. A mark answers with the update the Seen set of Rust would publish, and
 * `revealFails` makes the Finder refuse to reveal the Worktree.
 */
function rust(last: Update, revealFails = false, stopFails = false): Call[] {
  const calls: Call[] = [];
  mockIPC(
    (command, args) => {
      calls.push({ command, args });
      switch (command) {
        case "last_update":
          return last;
        case "follow_logs":
          return new Promise<void>(() => undefined);
        case "mark_seen":
          void emit("daemon-update", seen);
          return undefined;
        case "mark_unseen":
          void emit("daemon-update", unseen);
          return undefined;
        case "preview_release":
          return new Promise<void>(() => undefined);
        case "stop_service":
          return stopFails
            ? { response: "error", message: "web is held by something else" }
            : { response: "down", service: "web" };
        case "reveal_worktree":
          return revealFails
            ? Promise.reject({ kind: "reveal", message: `cannot reveal ${PATH} in the Finder: refused` })
            : undefined;
        case "refresh":
        case "stop_follow":
        case "set_menu_context":
        case "show_context_menu":
        case "open_worktree":
          return undefined;
        default:
          return Promise.reject(new Error(`unexpected command ${command}`));
      }
    },
    { shouldMockEvents: true },
  );
  return calls;
}

async function openWorktree(): Promise<HTMLElement> {
  render(<App store={createDaemonStore()} />);
  fireEvent.click(await screen.findByRole("button", { name: /^web-fix/ }));
  const center = screen.getByRole("main");
  await within(center).findByRole("heading", { name: "Services" });
  return center;
}

/** Right-clicks the row of `center` whose name matches `name`. */
function rightClick(center: HTMLElement, name: RegExp): void {
  fireEvent.contextMenu(within(center).getByRole("button", { name }));
}

/** The entries of the context menu the window asked Rust to show last. */
async function shownMenu(calls: Call[]): Promise<ContextEntry[]> {
  let entries: ContextEntry[] = [];
  await waitFor(() => {
    const shown = calls.filter((call) => call.command === "show_context_menu").at(-1);
    if (shown === undefined) {
      throw new Error("no context menu was shown");
    }
    entries = (shown.args as { entries: ContextEntry[] }).entries;
  });
  return entries;
}

function titles(entries: ContextEntry[]): string[] {
  return entries.map((entry) => (entry.kind === "separator" ? "---" : entry.title));
}

/** Chooses the entry titled `title` in `entries`, as Rust tells the window. */
async function choose(entries: ContextEntry[], title: string): Promise<void> {
  const index = titles(entries).indexOf(title);
  expect(index, title).toBeGreaterThanOrEqual(0);
  await act(async () => {
    await emit(CONTEXT_CHOICE, index);
  });
}

afterEach(() => {
  cleanup();
  clearMocks();
});

describe("the context menu of a row", () => {
  it("offers Mark as Seen on a failure that is not Seen, and selects the row", async () => {
    const calls = rust(unseen);
    rightClick(await openWorktree(), /sh -c/);

    const entries = await shownMenu(calls);

    expect(titles(entries)).toEqual(["Copy Command Line", "Copy Logs Command", "---", "Mark as Seen"]);
    expect(await screen.findByRole("heading", { name: "sh -c 'exit 3'" })).toBeInTheDocument();
    await choose(entries, "Mark as Seen");
    await waitFor(() => {
      expect(calls).toContainEqual({ command: "mark_seen", args: { key: JOB_KEY } });
    });
  });

  it("offers Mark as Unseen on a Seen row, which brings it back in Needs you and in the count", async () => {
    const calls = rust(seen);
    const sidebar = () => screen.getByRole("navigation", { name: "Worktrees" });
    rightClick(await openWorktree(), /sh -c/);
    expect(within(sidebar()).getByRole("button", { name: /^Overview/ })).toHaveTextContent("1 needs you");

    const entries = await shownMenu(calls);
    expect(titles(entries)).toEqual(["Copy Command Line", "Copy Logs Command", "---", "Mark as Unseen"]);
    await choose(entries, "Mark as Unseen");

    await waitFor(() => {
      expect(calls).toContainEqual({ command: "mark_unseen", args: { key: JOB_KEY } });
    });
    await waitFor(() => {
      expect(within(sidebar()).getByRole("button", { name: /^Overview/ })).toHaveTextContent("2 need you");
    });
    fireEvent.click(within(sidebar()).getByRole("button", { name: /^Overview/ }));
    const needs = await screen.findByRole("region", { name: "Needs you" });
    expect(within(needs).getByText(/sh -c 'exit 3'/)).toBeInTheDocument();
  });

  it("copies the port of a Service and its logs command, and offers no mark on what did not fail", async () => {
    const calls = rust(unseen);
    rightClick(await openWorktree(), /^web/);

    const entries = await shownMenu(calls);

    expect(entries.slice(0, 3)).toEqual([
      { kind: "copy", title: "Copy Port 18731", text: "18731" },
      { kind: "copy", title: "Copy Logs Command", text: "(cd /p/work/acme/web-fix && procly logs web)" },
      {
        kind: "copy",
        title: "Copy Restart Command",
        text: "(cd /p/work/acme/web-fix && procly restart web)",
      },
    ]);
    expect(titles(entries).some((title) => title.startsWith("Mark as"))).toBe(false);
  });

  it("copies the command line of a Job, which has no port", async () => {
    const calls = rust(unseen);
    rightClick(await openWorktree(), /sh -c/);

    const entries = await shownMenu(calls);

    expect(entries[0]).toEqual({ kind: "copy", title: "Copy Command Line", text: "sh -c 'exit 3'" });
  });

  it("offers Stop on a Service that runs, which sends Down for it from its Worktree", async () => {
    const calls = rust(unseen);
    rightClick(await openWorktree(), /^web/);

    const entries = await shownMenu(calls);

    expect(titles(entries).slice(-2)).toEqual(["---", "Stop"]);
    // Stop names the item of the menu bar whose key it shows, `⌘.`, and the other lines name none.
    expect(entries.at(-1)).toEqual({ kind: "act", title: "Stop", accelerator: "stop" });
    expect(entries.filter((entry) => "accelerator" in entry)).toHaveLength(1);
    await choose(entries, "Stop");
    await waitFor(() => {
      expect(calls).toContainEqual({ command: "stop_service", args: { service: "web", cwd: PATH } });
    });
  });

  it("says why a Stop was refused, where the failure of an opening shows", async () => {
    const calls = rust(unseen, false, true);
    rightClick(await openWorktree(), /^web/);

    await choose(await shownMenu(calls), "Stop");

    expect(await screen.findByText("web is held by something else")).toHaveAttribute("role", "alert");
  });

  it("offers no Stop on a Job, which only a Service or an Isolated Compose service has", async () => {
    const calls = rust(unseen);
    rightClick(await openWorktree(), /sh -c/);

    expect(titles(await shownMenu(calls))).not.toContain("Stop");
  });

  it("copies the command that stops a Foreign process and its port", async () => {
    const calls = rust(unseen);
    rightClick(await openWorktree(), /node server\.js/);

    const entries = await shownMenu(calls);

    expect(entries).toEqual([
      { kind: "copy", title: "Copy kill 4812", text: "kill 4812" },
      { kind: "copy", title: "Copy Port 3000", text: "3000" },
    ]);
  });
});

describe("the context menu of a Worktree in the sidebar", () => {
  it("offers the items of the Worktree menu for that Worktree", async () => {
    const calls = rust(unseen);
    render(<App store={createDaemonStore()} />);
    fireEvent.contextMenu(await screen.findByRole("button", { name: /^web-fix/ }));

    const entries = await shownMenu(calls);

    expect(titles(entries)).toEqual([
      "Open in Editor",
      "Open in Terminal",
      "Reveal in Finder",
      "---",
      "Copy Path",
      "Copy Slug",
      "---",
      "Check This Worktree",
      "---",
      "Release Worktree…",
    ]);
    expect(entries).toContainEqual({ kind: "copy", title: "Copy Path", text: PATH });
    expect(entries).toContainEqual({ kind: "copy", title: "Copy Slug", text: "web_fix" });
  });

  it("reveals in the Finder and releases the Worktree it was opened on", async () => {
    const calls = rust(unseen);
    render(<App store={createDaemonStore()} />);
    fireEvent.contextMenu(await screen.findByRole("button", { name: /^web-fix/ }));
    const entries = await shownMenu(calls);

    await choose(entries, "Reveal in Finder");
    await waitFor(() => {
      expect(calls).toContainEqual({ command: "reveal_worktree", args: { path: PATH } });
    });

    await choose(entries, "Release Worktree…");
    await waitFor(() => {
      expect(calls).toContainEqual({ command: "preview_release", args: { path: PATH } });
    });
  });

  it("checks the Worktree it was opened on: its screen opens and doctor runs from its directory", async () => {
    const calls = rust(unseen);
    render(<App store={createDaemonStore()} />);
    fireEvent.contextMenu(await screen.findByRole("button", { name: /^web-fix/ }));
    const entries = await shownMenu(calls);
    expect(
      calls.some((call) => call.command === "doctor" && (call.args as { cwd: unknown }).cwd === PATH),
    ).toBe(false);

    await choose(entries, "Check This Worktree");

    expect(
      await within(screen.getByRole("main")).findByRole("heading", { name: "web-fix" }),
    ).toBeInTheDocument();
    await waitFor(() => {
      expect(
        calls.some((call) => call.command === "doctor" && (call.args as { cwd: unknown }).cwd === PATH),
      ).toBe(true);
    });
  });

  it("says why the Finder did not reveal the Worktree, also on a screen that hides the inspector", async () => {
    const calls = rust(unseen, true);
    render(<App store={createDaemonStore()} />);
    const sidebar = screen.getByRole("navigation", { name: "Worktrees" });
    fireEvent.click(await within(sidebar).findByRole("button", { name: /^Orphans/ }));
    await screen.findByRole("heading", { name: "Orphans" });
    expect(screen.queryByRole("complementary", { name: "Inspector" })).toBeNull();

    fireEvent.contextMenu(within(sidebar).getByRole("button", { name: /^web-fix/ }));
    await choose(await shownMenu(calls), "Reveal in Finder");

    expect(await screen.findByText(/in the Finder: refused/)).toHaveAttribute("role", "alert");
  });
});
