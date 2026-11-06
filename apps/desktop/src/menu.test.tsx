import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { emit } from "@tauri-apps/api/event";
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { afterEach, describe, expect, it } from "vitest";

import { App } from "./App";
import { createDaemonStore, type NeedsYou, type Update } from "./daemon";
import { MENU_CHOICE, type MenuChoice } from "./menu";
import type { MenuContext } from "./protocol/MenuContext";
import type { Response } from "./protocol/Response";

const AT = Date.UTC(2026, 10, 5, 19, 35, 0);
const PATH = "/p/work/acme/web-fix";

/**
 * A Worktree with a Service the Daemon can restart, one it cannot, one that ended, an Isolated Compose service and a
 * Job that failed.
 */
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
            {
              service: "api",
              port: 18732,
              reattached: true,
              started_at: AT - 90_000,
              restartable: false,
              state: "healthy",
            },
            {
              service: "old",
              port: null,
              reattached: false,
              started_at: AT - 120_000,
              restartable: true,
              state: "ended",
              end: { kind: "exited", code: 0 },
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
          containers: [{ service: "cache", project: "web_fix", compose: "redis", port: 18740 }],
        },
      ],
    },
  ],
  jobs_outside_repositories: [],
};

/** The Job that failed, as "Needs you" lists it. */
const failedJob: NeedsYou = {
  kind: "job",
  worktree: PATH,
  id: "a4e2ea77",
  command: "sh -c 'exit 3'",
  end: { kind: "exited", code: 3 },
};

interface Call {
  command: string;
  args: unknown;
}

/** Plays the Rust side: every command the window sends is kept, the actions of the inspector succeed. */
function rust(): Call[] {
  const calls: Call[] = [];
  const update: Update = { at_ms: AT, response: scene, needs_you: [failedJob], seen: [] };
  mockIPC(
    (command, args) => {
      calls.push({ command, args });
      switch (command) {
        case "last_update":
          return update;
        case "follow_logs":
          return new Promise<void>(() => undefined);
        case "service_logs":
          return { response: "logs", output: "" };
        case "restart_service":
          return { response: "up", services: [] };
        case "stop_service":
          return { response: "down", stopped: [] };
        case "preview_release":
          return new Promise<void>(() => undefined);
        case "refresh":
        case "stop_follow":
        case "set_menu_context":
        case "mark_seen":
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

/** Picks the row of `center` that starts with `name`, which puts it in the inspector. */
function pick(center: HTMLElement, name: string | RegExp): void {
  fireEvent.click(
    within(center).getByRole("button", { name: typeof name === "string" ? new RegExp(`^${name}`) : name }),
  );
}

/** What the menu bar is told last. */
function lastContext(calls: Call[]): MenuContext | undefined {
  const sent = calls.filter((call) => call.command === "set_menu_context").at(-1);
  return (sent?.args as { context: MenuContext } | undefined)?.context;
}

async function choose(choice: MenuChoice): Promise<void> {
  await act(async () => {
    await emit(MENU_CHOICE, choice);
  });
}

afterEach(() => {
  cleanup();
  clearMocks();
});

describe("the menu bar", () => {
  it("goes to the Overview, Doctor and the Orphans from the View menu", async () => {
    rust();
    await openWorktree();

    await choose("overview");
    expect(await screen.findByRole("region", { name: "Overview" })).toBeInTheDocument();

    await choose("doctor");
    expect(await screen.findByRole("heading", { name: "Doctor" })).toBeInTheDocument();

    await choose("orphans");
    expect(await screen.findByRole("heading", { name: "Orphans" })).toBeInTheDocument();
  });

  it("is told what the selection allows, the Worktree items following the Worktree on screen", async () => {
    const calls = rust();
    const center = await openWorktree();

    await waitFor(() => {
      expect(lastContext(calls)).toEqual({
        logs_command: null,
        restart: false,
        stop: false,
        mark_seen: false,
        worktree: PATH,
      });
    });

    pick(center, "web");
    await waitFor(() => {
      expect(lastContext(calls)).toEqual({
        logs_command: "(cd /p/work/acme/web-fix && procly logs web)",
        restart: true,
        stop: true,
        mark_seen: false,
        worktree: PATH,
      });
    });

    // The Daemon holds no environment for `api`, so the inspector offers a command to copy and no Restart.
    pick(center, "api");
    await waitFor(() => {
      expect(lastContext(calls)).toMatchObject({ restart: false, stop: true });
    });

    pick(center, "old");
    await waitFor(() => {
      expect(lastContext(calls)).toMatchObject({ restart: true, stop: false, mark_seen: false });
    });

    pick(center, "cache");
    await waitFor(() => {
      expect(lastContext(calls)).toMatchObject({
        logs_command: "(cd /p/work/acme/web-fix && procly logs cache)",
        restart: false,
        stop: true,
      });
    });

    // Procly neither follows nor stops a Foreign process, so the menu offers it nothing but the Worktree.
    pick(center, /node server\.js/);
    await waitFor(() => {
      expect(lastContext(calls)).toEqual({
        logs_command: null,
        restart: false,
        stop: false,
        mark_seen: false,
        worktree: PATH,
      });
    });

    pick(center, /sh -c/);
    await waitFor(() => {
      expect(lastContext(calls)).toEqual({
        logs_command: "(cd /p/work/acme/web-fix && procly logs a4e2ea77)",
        restart: false,
        stop: false,
        mark_seen: true,
        worktree: PATH,
      });
    });
  });

  it("is told nothing while nothing is selected and no Worktree is open", async () => {
    const calls = rust();
    render(<App store={createDaemonStore()} />);
    await screen.findByRole("region", { name: "Overview" });

    expect(calls.map((call) => call.command)).not.toContain("set_menu_context");
  });

  it("restarts the selection as the inspector does, then asks for a fresh answer", async () => {
    const calls = rust();
    pick(await openWorktree(), "web");

    await choose("restart");

    await waitFor(() => {
      expect(calls).toContainEqual({ command: "restart_service", args: { service: "web", cwd: PATH } });
    });
    await waitFor(() => {
      expect(calls.map((call) => call.command)).toContain("refresh");
    });
  });

  it("restarts nothing the inspector offers no Restart for", async () => {
    const calls = rust();
    pick(await openWorktree(), "api");

    await choose("restart");
    await choose("stop");

    await waitFor(() => {
      expect(calls).toContainEqual({ command: "stop_service", args: { service: "api", cwd: PATH } });
    });
    expect(calls.map((call) => call.command)).not.toContain("restart_service");
  });

  it("marks the selected failure as Seen", async () => {
    const calls = rust();
    pick(await openWorktree(), /sh -c/);

    await choose("mark-seen");

    await waitFor(() => {
      expect(calls).toContainEqual({ command: "mark_seen", args: { key: { kind: "job", id: "a4e2ea77" } } });
    });
  });

  it("opens the Worktree in the editor and starts its release from the Worktree menu", async () => {
    const calls = rust();
    await openWorktree();

    await choose("open-in-editor");
    await waitFor(() => {
      expect(calls).toContainEqual({ command: "open_worktree", args: { path: PATH, with: "editor" } });
    });

    await choose("release");
    await waitFor(() => {
      expect(calls).toContainEqual({ command: "preview_release", args: { path: PATH } });
    });
  });

  it("opens the Worktree of a Foreign process from the Worktree menu, though its inspector has no buttons", async () => {
    const calls = rust();
    const center = await openWorktree();
    pick(center, /node server\.js/);
    await waitFor(() => {
      expect(screen.getByRole("heading", { name: "node server.js" })).toBeInTheDocument();
    });

    await choose("open-in-terminal");

    await waitFor(() => {
      expect(calls).toContainEqual({ command: "open_worktree", args: { path: PATH, with: "terminal" } });
    });
  });
});
