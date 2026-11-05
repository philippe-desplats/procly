import { cleanup, render, screen, within } from "@testing-library/react";
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { afterEach, describe, expect, it } from "vitest";

import { App } from "./App";
import { createDaemonStore } from "./daemon";
import recorded from "./fixtures/status.json";
import type { Response } from "./protocol/Response";

/** A `Status { all: true }` answer recorded from a running Daemon, its paths replaced by neutral ones. */
const status: Response = recorded as Response;

/** Answers `status` with `answer` and records every command the window sends. */
function daemon(answer: () => Promise<Response>): string[] {
  const commands: string[] = [];
  mockIPC((command) => {
    commands.push(command);
    return command === "status" ? answer() : Promise.reject(new Error(`unexpected command ${command}`));
  });
  return commands;
}

describe("the window", () => {
  afterEach(() => {
    cleanup();
    clearMocks();
  });

  it("draws the 3 panes and the Worktrees of the recorded answer", async () => {
    const commands = daemon(() => Promise.resolve(status));

    render(<App store={createDaemonStore()} />);

    const sidebar = screen.getByRole("navigation", { name: "Worktrees" });
    expect(await within(sidebar).findByRole("button", { name: /^web\s*main$/ })).toBeInTheDocument();
    expect(within(sidebar).getByRole("button", { name: "web-fix" })).toBeInTheDocument();
    expect(within(sidebar).getByRole("heading", { name: "web" })).toBeInTheDocument();
    expect(screen.getByRole("main")).toBeInTheDocument();
    expect(screen.getByRole("complementary", { name: "Inspector" })).toBeInTheDocument();
    // The first Worktree is open: its Service and Job are in the center, its path in the inspector.
    const center = screen.getByRole("main");
    expect(within(center).getByText("api")).toBeInTheDocument();
    expect(within(center).getByText("healthy")).toBeInTheDocument();
    expect(within(center).getByText("sleep 60")).toBeInTheDocument();
    expect(screen.getByText("/p/work/acme/web")).toBeInTheDocument();
    expect(commands).toContain("status");
  });

  it("opens another Worktree from the sidebar", async () => {
    daemon(() => Promise.resolve(status));
    render(<App store={createDaemonStore()} />);

    (await screen.findByRole("button", { name: "web-fix" })).click();

    const center = screen.getByRole("main");
    expect(await within(center).findByText("exited 3")).toBeInTheDocument();
    expect(screen.getByText("/p/work/acme/web-fix")).toBeInTheDocument();
  });

  it("says that no Daemon runs, and sends nothing but the status request", async () => {
    const commands = daemon(() =>
      Promise.reject({ kind: "unreachable", message: "the daemon did not answer on /p/state/procly.sock" }),
    );

    render(<App store={createDaemonStore()} />);

    expect(await screen.findByRole("heading", { name: "Daemon not running" })).toBeInTheDocument();
    expect(screen.getByText(/does not start one/)).toBeInTheDocument();
    expect(commands).toEqual(["status"]);
  });

  it("shows why a Daemon that does not answer well failed", async () => {
    daemon(() => Promise.reject({ kind: "decode", message: "unreadable answer from the daemon" }));

    render(<App store={createDaemonStore()} />);

    expect(await screen.findByText("unreadable answer from the daemon")).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "Daemon not running" })).not.toBeInTheDocument();
  });
});
