import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { afterEach, describe, expect, it } from "vitest";

import { App } from "./App";
import { createDaemonStore, type Update } from "./daemon";
import recordedGc from "./fixtures/gc.json";
import recordedStatus from "./fixtures/status.json";
import type { Response } from "./protocol/Response";

/** A `Gc { apply: false }` answer in the shape the Daemon gives it, its paths replaced by neutral ones. */
const gc: Response = recordedGc as Response;

const FIXED: Update = {
  at_ms: Date.UTC(2026, 10, 5, 19, 35, 0),
  response: recordedStatus as Response,
  needs_you: [],
  seen: [],
};

interface Call {
  command: string;
  args: unknown;
}

/** Plays the Rust side: `listing` answers the dry run, `deleting` answers the collection, every command is kept. */
function app(options: { listing?: () => unknown; deleting?: (args: unknown) => unknown } = {}): Call[] {
  const calls: Call[] = [];
  mockIPC(
    (command, args) => {
      calls.push({ command, args });
      switch (command) {
        case "last_update":
          return FIXED;
        case "list_orphans":
          return options.listing?.() ?? gc;
        case "delete_orphans":
          return options.deleting?.(args);
        default:
          return Promise.reject(new Error(`unexpected command ${command}`));
      }
    },
    { shouldMockEvents: true },
  );
  return calls;
}

function orphansOf(response: Response) {
  if (response.response !== "gc") {
    throw new Error("not a Gc answer");
  }
  return response.orphans;
}

async function openOrphans(): Promise<void> {
  fireEvent.click(await screen.findByRole("button", { name: /^Orphans/ }));
  await screen.findByRole("heading", { name: "Orphans" });
}

describe("the Orphans screen", () => {
  afterEach(() => {
    cleanup();
    clearMocks();
  });

  it("is an entry of the sidebar above Settings, and replaces the inspector", async () => {
    app();
    render(<App store={createDaemonStore()} />);

    const sidebar = await screen.findByRole("navigation", { name: "Worktrees" });
    const entries = within(sidebar).getAllByRole("button");
    expect(entries.at(-2)).toHaveAccessibleName(/^Orphans/);
    expect(entries.at(-1)).toHaveAccessibleName(/^Settings/);

    fireEvent.click(entries.at(-2)!);

    expect(await screen.findByRole("heading", { name: "Orphans" })).toBeInTheDocument();
    expect(screen.queryByRole("complementary", { name: "Inspector" })).not.toBeInTheDocument();
  });

  it("runs a dry run when it opens, never an apply", async () => {
    const calls = app();
    render(<App store={createDaemonStore()} />);

    await openOrphans();

    await screen.findByRole("heading", { name: "web-login" });
    expect(calls.filter((call) => call.command === "list_orphans")).toHaveLength(1);
    expect(calls.some((call) => call.command === "delete_orphans")).toBe(false);
  });

  it("groups the Orphans of the recorded answer by removed Worktree and names the data each one drops", async () => {
    app();
    render(<App store={createDaemonStore()} />);

    await openOrphans();

    const login = within(await screen.findByRole("region", { name: "web-login" }));
    expect(login.getAllByRole("listitem")).toHaveLength(5);
    expect(login.getByText("/p/work/acme/web-login")).toBeInTheDocument();
    expect(login.getByText(/Returns port 24081 to the Range/)).toBeInTheDocument();
    expect(
      login.getByText(/Drops web__web_login in its postgres container, with the data in it/),
    ).toBeInTheDocument();
    expect(login.getByText(/Deletes \/p\/state\/output\/web_login-5c2e0d6b8a41f7e3/)).toBeInTheDocument();
    expect(login.getByText(/Removes the Isolated container of cache/)).toBeInTheDocument();
    expect(
      login.getByText(
        /Removes the volume web-web_login_data of compose project web-web_login, with the data in it/,
      ),
    ).toBeInTheDocument();
    const search = within(screen.getByRole("region", { name: "web-search" }));
    expect(search.getAllByRole("listitem")).toHaveLength(1);
  });

  it("says so when nothing is an Orphan, and offers no deletion", async () => {
    app({ listing: () => ({ ...recordedGc, orphans: [] }) });
    render(<App store={createDaemonStore()} />);

    await openOrphans();

    expect(await screen.findByText("No Orphan.")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /^Delete/ })).not.toBeInTheDocument();
  });

  it("shows why the Daemon could not be asked", async () => {
    app({ listing: () => Promise.reject({ kind: "unreachable", message: "no Daemon answers" }) });
    render(<App store={createDaemonStore()} />);

    await openOrphans();

    expect(await screen.findByRole("alert")).toHaveTextContent("no Daemon answers");
  });

  it("opens a sheet with Cancel as the default button, naming the loss and the CLI equivalent", async () => {
    app();
    render(<App store={createDaemonStore()} />);
    await openOrphans();

    fireEvent.click(await screen.findByRole("button", { name: /^Delete 6 Orphans/ }));

    const sheet = within(await screen.findByRole("dialog"));
    const cancel = sheet.getByRole("button", { name: "Cancel" });
    expect(cancel).toHaveFocus();
    expect(cancel).toHaveClass("default");
    expect(sheet.getByRole("button", { name: "Delete 6 Orphans" })).not.toHaveClass("default");
    expect(sheet.getByText(/cannot be undone/)).toBeInTheDocument();
    expect(sheet.getByText("procly gc --apply")).toBeInTheDocument();
  });

  it("names in the sheet itself how many Orphans of each kind it deletes and the data they drop", async () => {
    app();
    render(<App store={createDaemonStore()} />);
    await openOrphans();

    fireEvent.click(await screen.findByRole("button", { name: /^Delete 6 Orphans/ }));

    const sheet = within(await screen.findByRole("dialog"));
    const effects = sheet.getByRole("list", { name: "What it deletes" }).querySelectorAll("li");
    expect([...effects].map((effect) => effect.textContent)).toEqual([
      "2 Leases: their ports go back to the Range, with their Portless aliases.",
      "1 Logical resource: dropped in its container, with the data in it.",
      "1 output directory: deleted, with the output files of its Services.",
      "1 Isolated container: removed, with its anonymous volumes.",
      "1 volume: removed, with the data in it.",
    ]);
    // The sheet says it all, it never sends the developer to the screen behind it.
    expect(sheet.queryByText(/on the screen/)).not.toBeInTheDocument();
  });

  it("deletes with `only` equal to the Orphans it showed, and sends nothing when cancelled", async () => {
    const calls = app({ deleting: () => ({ ...recordedGc, applied: true }) });
    render(<App store={createDaemonStore()} />);
    await openOrphans();

    fireEvent.click(await screen.findByRole("button", { name: /^Delete 6 Orphans/ }));
    fireEvent.click(within(await screen.findByRole("dialog")).getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(calls.some((call) => call.command === "delete_orphans")).toBe(false);

    fireEvent.click(screen.getByRole("button", { name: /^Delete 6 Orphans/ }));
    fireEvent.click(
      within(await screen.findByRole("dialog")).getByRole("button", { name: "Delete 6 Orphans" }),
    );

    await waitFor(() => {
      expect(calls.filter((call) => call.command === "delete_orphans")).toHaveLength(1);
    });
    const sent = calls.find((call) => call.command === "delete_orphans");
    expect(sent?.args).toEqual({ only: orphansOf(gc) });
  });

  it("shows the result item by item, with the reason of an Orphan that was not deleted", async () => {
    const orphans = orphansOf(gc).map((orphan, index) =>
      index === 1
        ? { ...orphan, error: "no longer an Orphan" }
        : index === 2
          ? { ...orphan, error: "cannot remove the directory: Permission denied" }
          : { ...orphan, deleted: true },
    );
    app({ deleting: () => ({ ...recordedGc, applied: true, orphans }) });
    render(<App store={createDaemonStore()} />);
    await openOrphans();

    fireEvent.click(await screen.findByRole("button", { name: /^Delete 6 Orphans/ }));
    fireEvent.click(
      within(await screen.findByRole("dialog")).getByRole("button", { name: "Delete 6 Orphans" }),
    );

    const result = within(await screen.findByRole("dialog", { name: "Result" }));
    const items = result.getAllByRole("listitem");
    expect(items).toHaveLength(6);
    expect(items[0]).toHaveTextContent("Deleted");
    expect(items[1]).toHaveTextContent("Not deleted");
    expect(items[1]).toHaveTextContent("no longer an Orphan");
    expect(items[2]).toHaveTextContent("Not deleted");
    expect(items[2]).toHaveTextContent("Permission denied");
    expect(result.getByRole("button", { name: "Done" })).toHaveClass("default");
  });

  it("looks again after the result is closed", async () => {
    const calls = app({ deleting: () => ({ ...recordedGc, applied: true }) });
    render(<App store={createDaemonStore()} />);
    await openOrphans();
    fireEvent.click(await screen.findByRole("button", { name: /^Delete 6 Orphans/ }));
    fireEvent.click(
      within(await screen.findByRole("dialog")).getByRole("button", { name: "Delete 6 Orphans" }),
    );

    fireEvent.click(
      await within(await screen.findByRole("dialog", { name: "Result" })).findByRole("button", {
        name: "Done",
      }),
    );

    await waitFor(() => {
      expect(calls.filter((call) => call.command === "list_orphans")).toHaveLength(2);
    });
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });

  it("shows the error when the collection fails, and keeps the sheet to try again or cancel", async () => {
    app({ deleting: () => Promise.reject({ kind: "unreachable", message: "no Daemon answers" }) });
    render(<App store={createDaemonStore()} />);
    await openOrphans();

    fireEvent.click(await screen.findByRole("button", { name: /^Delete 6 Orphans/ }));
    fireEvent.click(
      within(await screen.findByRole("dialog")).getByRole("button", { name: "Delete 6 Orphans" }),
    );

    const sheet = within(await screen.findByRole("dialog"));
    expect(await sheet.findByRole("alert")).toHaveTextContent("no Daemon answers");
    expect(sheet.getByRole("button", { name: "Cancel" })).toBeEnabled();
  });
});
