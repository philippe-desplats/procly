import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { afterEach, describe, expect, it } from "vitest";

import { App } from "./App";
import { createDaemonStore, type Update } from "./daemon";
import recordedRelease from "./fixtures/release.json";
import type { Response } from "./protocol/Response";

const AT = Date.UTC(2026, 10, 6, 10, 0, 0);
const PATH = "/p/work/acme/web-fix";
const MAIN = "/p/work/acme/web";

/** A dry run in the shape the Daemon gives it, its paths replaced by neutral ones. */
const dryRun: Response = recordedRelease as Response;

function scene(options: { listed: boolean }): Response {
  return {
    response: "status",
    repositories: [
      {
        git_common_dir: "/p/work/acme/web/.git",
        worktrees: [
          {
            path: MAIN,
            slug: "web",
            main: true,
            branch: "main",
            listed: true,
            services: [],
            jobs: [],
          },
          {
            path: PATH,
            slug: "web_fix",
            main: false,
            branch: "web-fix",
            listed: options.listed,
            services: [
              {
                service: "web",
                port: 24001,
                reattached: false,
                started_at: AT - 60_000,
                restartable: true,
                state: "healthy",
              },
            ],
            jobs: [],
          },
        ],
      },
    ],
    jobs_outside_repositories: [],
  };
}

interface Call {
  command: string;
  args: unknown;
}

/** Plays the Rust side: `previewing` answers the dry run, `releasing` the real release, every command is kept. */
function app(
  options: {
    listed?: boolean;
    previewing?: () => unknown;
    releasing?: () => unknown;
  } = {},
): Call[] {
  const calls: Call[] = [];
  const update: Update = {
    at_ms: AT,
    response: scene({ listed: options.listed ?? true }),
    needs_you: [],
    seen: [],
  };
  mockIPC(
    (command, args) => {
      calls.push({ command, args });
      switch (command) {
        case "last_update":
          return update;
        case "refresh":
        case "stop_follow":
          return undefined;
        case "follow_logs":
          return new Promise<void>(() => undefined);
        case "service_logs":
          return { response: "logs", output: "" };
        case "preview_release":
          return options.previewing?.() ?? dryRun;
        case "release_worktree":
          return options.releasing?.();
        default:
          return Promise.reject(new Error(`unexpected command ${command}`));
      }
    },
    { shouldMockEvents: true },
  );
  return calls;
}

async function openWorktree(name: RegExp): Promise<HTMLElement> {
  render(<App store={createDaemonStore()} />);
  fireEvent.click(await screen.findByRole("button", { name }));
  return screen.getByRole("main");
}

/** Opens the sheet of the Worktree `web-fix`. */
async function openSheet(): Promise<HTMLElement> {
  const center = await openWorktree(/^web-fix/);
  fireEvent.click(await within(center).findByRole("button", { name: "Release…" }));
  return screen.findByRole("dialog");
}

const emptyReport = {
  response: "worktree_release",
  worktree: PATH,
  services: [],
  jobs: [],
  compose_services: [],
  leases: [],
  kept_leases: [],
  logical_resources: [],
  warnings: [],
};

describe("the Worktree release sheet", () => {
  afterEach(() => {
    cleanup();
    clearMocks();
  });

  it("is offered on every Worktree, the Main worktree included", async () => {
    app();
    render(<App store={createDaemonStore()} />);

    fireEvent.click(await screen.findByRole("button", { name: /^web-fix/ }));
    expect(await screen.findByRole("button", { name: "Release…" })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: /^main/ }));
    expect(await screen.findByRole("button", { name: "Release…" })).toBeInTheDocument();
  });

  it("sends one dry run when it opens and no real release", async () => {
    const calls = app();

    await openSheet();

    await waitFor(() => {
      expect(calls.filter((call) => call.command === "preview_release")).toEqual([
        { command: "preview_release", args: { path: PATH } },
      ]);
    });
    expect(calls.some((call) => call.command === "release_worktree")).toBe(false);
  });

  it("lists what stops, what is released and what is kept, with the reason of each Lease kept", async () => {
    app();

    const sheet = within(await openSheet());

    expect(await sheet.findByRole("heading", { name: "Will stop" })).toBeInTheDocument();
    expect(sheet.getByText("web")).toBeInTheDocument();
    expect(sheet.getByText("cache")).toBeInTheDocument();
    expect(sheet.getByText("Job ea79fa81")).toBeInTheDocument();
    expect(sheet.getByRole("heading", { name: "Will let go of" })).toBeInTheDocument();
    expect(sheet.getByText("db: its container keeps running for 1 other Worktree")).toBeInTheDocument();
    expect(sheet.getByRole("heading", { name: "Will release" })).toBeInTheDocument();
    expect(sheet.getByText("Lease web.http, port 24001")).toBeInTheDocument();
    expect(sheet.getByText("Lease api.http, port 24002")).toBeInTheDocument();
    expect(sheet.getByRole("heading", { name: "Kept" })).toBeInTheDocument();
    expect(
      sheet.getByText("Lease web.debug, port 24003, kept for Pinned branch web-fix"),
    ).toBeInTheDocument();
    expect(
      sheet.getByText("Lease db.tcp, port 24004, kept because its container still runs"),
    ).toBeInTheDocument();
    expect(sheet.getByText("Logical resource web__web_fix of db (postgres)")).toBeInTheDocument();
  });

  it("says that ended Jobs and Services are cleared and that no data is deleted", async () => {
    app();

    const sheet = within(await openSheet());

    expect(
      await sheet.findByText(/ended Jobs and Services of this Worktree are cleared/),
    ).toBeInTheDocument();
    expect(sheet.getByText(/No data is deleted/)).toBeInTheDocument();
  });

  it("warns first when git still lists the Worktree", async () => {
    app({ listed: true });

    const dialog = await openSheet();

    const warning = within(dialog).getByRole("alert");
    expect(warning).toHaveTextContent(/Git still lists this Worktree/);
    // The warning comes before any effect.
    const order = Array.from(dialog.querySelectorAll("p, h3"));
    expect(order[0]).toBe(warning);
  });

  it("does not warn about a Worktree git no longer lists", async () => {
    app({ listed: false });

    const sheet = within(await openSheet());

    await sheet.findByRole("heading", { name: "Will stop" });
    expect(sheet.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("has Cancel as the default button and shows the CLI equivalent", async () => {
    app();

    const sheet = within(await openSheet());

    const cancel = sheet.getByRole("button", { name: "Cancel" });
    expect(cancel).toHaveFocus();
    expect(cancel).toHaveClass("default");
    expect(await sheet.findByRole("button", { name: "Release" })).not.toHaveClass("default");
    expect(sheet.getByText(`(cd ${PATH} && procly worktree release)`)).toBeInTheDocument();
  });

  it("disables Release when the report says Procly holds nothing, kept Leases not counting", async () => {
    app({
      previewing: () => ({
        ...emptyReport,
        kept_leases: [
          { service: "web", port_name: "debug", port: 24003, reason: "pinned_branch", branch: "web-fix" },
        ],
        logical_resources: [{ service: "db", driver: "postgres", resource: "dev", name: "web__web_fix" }],
      }),
    });

    const sheet = within(await openSheet());

    expect(await sheet.findByText(/Procly holds nothing in this Worktree/)).toBeInTheDocument();
    expect(sheet.getByRole("button", { name: "Release" })).toBeDisabled();
    expect(sheet.getByRole("heading", { name: "Kept" })).toBeInTheDocument();
  });

  it("disables Release while the dry run is read and when it failed", async () => {
    app({ previewing: () => Promise.reject({ kind: "unreachable", message: "no Daemon answers" }) });

    const sheet = within(await openSheet());

    expect(await sheet.findByText("no Daemon answers")).toBeInTheDocument();
    expect(sheet.getByRole("button", { name: "Release" })).toBeDisabled();
    expect(sheet.getByRole("button", { name: "Cancel" })).toBeEnabled();
  });

  it("sends nothing when cancelled", async () => {
    const calls = app();
    const sheet = within(await openSheet());
    await sheet.findByRole("heading", { name: "Will stop" });

    fireEvent.click(sheet.getByRole("button", { name: "Cancel" }));

    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(calls.some((call) => call.command === "release_worktree")).toBe(false);
  });

  it("closes on Escape", async () => {
    app();
    const dialog = await openSheet();
    await within(dialog).findByRole("heading", { name: "Will stop" });

    fireEvent.keyDown(dialog, { key: "Escape" });

    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });

  it("confirms with the real release and shows the report it returns, not the dry run", async () => {
    const calls = app({
      releasing: () => ({
        ...emptyReport,
        services: ["web"],
        leases: [{ service: "web", port_name: "http", port: 24001 }],
      }),
    });
    const sheet = within(await openSheet());
    fireEvent.click(await sheet.findByRole("button", { name: "Release" }));

    const result = within(await screen.findByRole("dialog", { name: "Released web-fix" }));

    expect(calls.filter((call) => call.command === "release_worktree")).toEqual([
      { command: "release_worktree", args: { path: PATH } },
    ]);
    expect(result.getByRole("heading", { name: "Stopped" })).toBeInTheDocument();
    expect(result.getByText("web")).toBeInTheDocument();
    expect(result.getByRole("heading", { name: "Released" })).toBeInTheDocument();
    expect(result.getByText("Lease web.http, port 24001")).toBeInTheDocument();
    // What the dry run listed and the real release did not do is not shown, nor flagged.
    expect(result.queryByText("cache")).not.toBeInTheDocument();
    expect(result.queryByText(/Job ea79fa81/)).not.toBeInTheDocument();
    expect(result.getByRole("button", { name: "Done" })).toHaveClass("default");
    await waitFor(() => {
      expect(calls.map((call) => call.command)).toContain("refresh");
    });
  });

  it("shows why the release failed and keeps the sheet to try again or cancel", async () => {
    app({ releasing: () => Promise.reject({ kind: "unreachable", message: "no Daemon answers" }) });
    const sheet = within(await openSheet());
    fireEvent.click(await sheet.findByRole("button", { name: "Release" }));

    expect(await sheet.findByText("no Daemon answers")).toBeInTheDocument();
    expect(sheet.getByRole("button", { name: "Release" })).toBeEnabled();
    expect(sheet.getByRole("button", { name: "Cancel" })).toBeEnabled();
  });

  it("closes the result with Done", async () => {
    app({ releasing: () => ({ ...emptyReport, services: ["web"] }) });
    const sheet = within(await openSheet());
    fireEvent.click(await sheet.findByRole("button", { name: "Release" }));

    fireEvent.click(
      await within(await screen.findByRole("dialog", { name: "Released web-fix" })).findByRole("button", {
        name: "Done",
      }),
    );

    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });
});
