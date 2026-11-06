import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { emit } from "@tauri-apps/api/event";
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { afterEach, describe, expect, it } from "vitest";

import { App } from "./App";
import { createDaemonStore, type Failure, type Update } from "./daemon";
import recorded from "./fixtures/status.json";
import type { Response } from "./protocol/Response";

const status: Response = recorded as Response;

const AT = Date.UTC(2026, 10, 5, 19, 35, 0);

const NOT_RUNNING: Failure = {
  kind: "unreachable",
  message: "the daemon did not answer on /p/state/procly.sock",
};

function mismatch(daemon: number, app = 22): Failure {
  return {
    kind: "protocol_mismatch",
    message: `protocol version ${daemon} is not supported, this procly speaks version ${app}`,
    daemon_protocol: daemon,
    app_protocol: app,
  };
}

/** A clipboard that keeps what is written to it. */
function clipboard(): string[] {
  const written: string[] = [];
  Object.defineProperty(navigator, "clipboard", {
    configurable: true,
    value: {
      writeText: (text: string) => {
        written.push(text);
        return Promise.resolve();
      },
    },
  });
  return written;
}

/** The 3 parts of the 3-pane shell, which a Daemon that cannot be read leaves nothing to put in. */
function expectNoShell(): void {
  expect(screen.queryByRole("navigation", { name: "Worktrees" })).not.toBeInTheDocument();
  expect(screen.queryByRole("complementary", { name: "Inspector" })).not.toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Refresh" })).not.toBeInTheDocument();
  expect(screen.queryByText(/^Updated /)).not.toBeInTheDocument();
}

/** The brand mark and the title of a full-window state screen, in the one centered column. */
function expectCenteredScreen(title: string): void {
  const column = screen.getByRole("main");
  expect(column).toHaveClass("state-screen");
  expect(within(column).getByRole("img", { name: "Procly" })).toBeInTheDocument();
  expect(within(column).getByRole("heading", { name: title })).toBeInTheDocument();
}

function answered(at = AT): Update {
  return { at_ms: at, response: status, needs_you: [], seen: [] };
}

function failed(failure: Failure): Update {
  return { at_ms: AT, failure, needs_you: [], seen: [] };
}

/**
 * Plays the Rust side. `last` is what the poller already answered, `handlers` answer the commands of the lifecycle,
 * and every command the window sends is kept, with its arguments.
 */
function rust(last: Update | null, handlers: Record<string, () => unknown> = {}): string[] {
  const commands: string[] = [];
  mockIPC(
    (command) => {
      commands.push(command);
      if (command === "last_update") {
        return last;
      }
      const handler = handlers[command];
      return handler === undefined ? Promise.reject(new Error(`unexpected command ${command}`)) : handler();
    },
    { shouldMockEvents: true },
  );
  return commands;
}

async function tick(update: Update): Promise<void> {
  await act(async () => {
    await emit("daemon-update", update);
  });
}

describe("a Daemon that does not run", () => {
  afterEach(() => {
    cleanup();
    clearMocks();
  });

  it("offers to start it and the command to install the Session unit, and starts nothing by itself", async () => {
    const commands = rust(failed(NOT_RUNNING));

    render(<App store={createDaemonStore()} />);

    expect(await screen.findByRole("heading", { name: "The Daemon is not running" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Start the Daemon" })).toBeEnabled();
    expect(screen.getByText("procly unit install")).toBeInTheDocument();
    // Doctor ran on entering the state, which starts nothing.
    expect(commands).toEqual(["last_update", "doctor"]);
  });

  it("takes the whole window, with the brand mark and the title, and no sidebar, toolbar or inspector", async () => {
    rust(failed(NOT_RUNNING));

    render(<App store={createDaemonStore()} />);

    await screen.findByRole("heading", { name: "The Daemon is not running" });
    expectCenteredScreen("The Daemon is not running");
    expectNoShell();
    expect(screen.getByRole("button", { name: "Start the Daemon" })).toHaveClass("brand");
    expect(screen.getByRole("region", { name: "Checks without the Daemon" })).toBeInTheDocument();
  });

  it("copies exactly the command it shows", async () => {
    rust(failed(NOT_RUNNING));
    const written = clipboard();
    render(<App store={createDaemonStore()} />);

    fireEvent.click(await screen.findByRole("button", { name: "Copy" }));

    expect(written).toEqual(["procly unit install"]);
  });

  it("starts the Daemon when the developer asks, and the window recovers on the answer that follows", async () => {
    const commands = rust(failed(NOT_RUNNING), { start_daemon: () => undefined });
    render(<App store={createDaemonStore()} />);

    fireEvent.click(await screen.findByRole("button", { name: "Start the Daemon" }));
    await act(async () => {
      await Promise.resolve();
    });
    expect(commands).toEqual(["last_update", "doctor", "start_daemon"]);

    await tick(answered());
    expect(await screen.findByRole("region", { name: "Overview" })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "The Daemon is not running" })).not.toBeInTheDocument();
  });

  it("shows why the Daemon could not be started, and lets the developer try again", async () => {
    rust(failed(NOT_RUNNING), {
      start_daemon: () =>
        Promise.reject({
          kind: "start_daemon",
          message: "cannot start the daemon with /p/app/procly daemon",
        }),
    });
    render(<App store={createDaemonStore()} />);

    fireEvent.click(await screen.findByRole("button", { name: "Start the Daemon" }));

    expect(await screen.findByText("cannot start the daemon with /p/app/procly daemon")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Start the Daemon" })).toBeEnabled();
  });

  it("follows the Daemon down when it is stopped from the CLI, and back once it runs again", async () => {
    rust(answered());
    render(<App store={createDaemonStore()} />);
    expect(await screen.findByRole("region", { name: "Overview" })).toBeInTheDocument();

    await tick(failed(NOT_RUNNING));
    expect(await screen.findByRole("heading", { name: "The Daemon is not running" })).toBeInTheDocument();

    await tick(answered(AT + 5000));
    expect(await screen.findByRole("region", { name: "Overview" })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "The Daemon is not running" })).not.toBeInTheDocument();
  });
});

describe("a Daemon of another protocol version", () => {
  afterEach(() => {
    cleanup();
    clearMocks();
  });

  it("blocks on an older Daemon, naming both versions, and offers to restart it", async () => {
    rust(failed(mismatch(21)));

    render(<App store={createDaemonStore()} />);

    expect(
      await screen.findByRole("heading", { name: "The Daemon runs an older version" }),
    ).toBeInTheDocument();
    expect(screen.getByText("The Daemon speaks protocol 21, this app speaks 22.")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Restart the Daemon" })).toBeEnabled();
  });

  it("takes the whole window on an older Daemon, with the brand mark and the title", async () => {
    rust(failed(mismatch(21)));

    render(<App store={createDaemonStore()} />);

    await screen.findByRole("heading", { name: "The Daemon runs an older version" });
    expectCenteredScreen("The Daemon runs an older version");
    expectNoShell();
  });

  it("takes the whole window on a newer Daemon, with the brand mark and the title", async () => {
    rust(failed(mismatch(23)));

    render(<App store={createDaemonStore()} />);

    await screen.findByRole("heading", { name: "The Daemon runs a newer version" });
    expectCenteredScreen("The Daemon runs a newer version");
    expectNoShell();
  });

  it("warns on the screen of an older Daemon what a restart stops, before any sheet opens", async () => {
    rust(failed(mismatch(21)));

    render(<App store={createDaemonStore()} />);

    const warning = await screen.findByText("Restarting it stops every Service and Job of every Repository.");
    expect(warning).toBeInTheDocument();
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });

  it("shows the command that stops an older Daemon on the screen, and copies exactly that", async () => {
    rust(failed(mismatch(21)));
    const written = clipboard();
    render(<App store={createDaemonStore()} />);

    expect(await screen.findByText("procly daemon stop")).toBeInTheDocument();
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Copy" }));

    expect(written).toEqual(["procly daemon stop"]);
  });

  it("shows the command that updates the app on a newer Daemon, and copies exactly that", async () => {
    rust(failed(mismatch(23)));
    const written = clipboard();
    render(<App store={createDaemonStore()} />);

    expect(await screen.findByText("brew upgrade --cask procly")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Copy" }));

    expect(written).toEqual(["brew upgrade --cask procly"]);
  });

  it("holds the checks that run without the Daemon outside the narrow column of the explanation", async () => {
    rust(failed(NOT_RUNNING));

    render(<App store={createDaemonStore()} />);

    const column = (await screen.findByRole("heading", { name: "The Daemon is not running" })).parentElement;
    const checks = screen.getByRole("region", { name: "Checks without the Daemon" });
    expect(column).toHaveClass("state-column");
    expect(column).not.toContainElement(checks);
    expect(screen.getByRole("main")).toContainElement(checks);
  });

  it("brings the shell back once the Daemon answers in the protocol of the app", async () => {
    rust(failed(mismatch(21)));
    render(<App store={createDaemonStore()} />);
    await screen.findByRole("heading", { name: "The Daemon runs an older version" });

    await tick(answered());

    expect(await screen.findByRole("region", { name: "Overview" })).toBeInTheDocument();
    expect(screen.getByRole("navigation", { name: "Worktrees" })).toBeInTheDocument();
    expect(screen.getByRole("complementary", { name: "Inspector" })).toBeInTheDocument();
    expect(
      screen.queryByRole("heading", { name: "The Daemon runs an older version" }),
    ).not.toBeInTheDocument();
  });

  it("asks for an update of the app on a newer Daemon, and offers no restart", async () => {
    rust(failed(mismatch(23)));

    render(<App store={createDaemonStore()} />);

    expect(
      await screen.findByRole("heading", { name: "The Daemon runs a newer version" }),
    ).toBeInTheDocument();
    expect(screen.getByText("The Daemon speaks protocol 23, this app speaks 22.")).toBeInTheDocument();
    expect(screen.getByText(/update Procly/i)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /restart/i })).not.toBeInTheDocument();
  });

  it("warns that every running Service will stop behind a sheet where Cancel is the default", async () => {
    const commands = rust(failed(mismatch(21)));
    render(<App store={createDaemonStore()} />);

    fireEvent.click(await screen.findByRole("button", { name: "Restart the Daemon" }));

    const sheet = await screen.findByRole("dialog", { name: "Restart the Daemon" });
    expect(within(sheet).getByText(/every running Service will stop/i)).toBeInTheDocument();
    expect(within(sheet).getByText("procly daemon stop")).toBeInTheDocument();
    const cancel = within(sheet).getByRole("button", { name: "Cancel" });
    expect(cancel).toHaveFocus();
    expect(cancel).toHaveClass("default");
    expect(within(sheet).getByRole("button", { name: "Restart the Daemon" })).not.toHaveClass("default");
    expect(commands).toEqual(["last_update"]);
  });

  it("restarts nothing when the developer cancels, or presses Escape", async () => {
    const commands = rust(failed(mismatch(21)));
    render(<App store={createDaemonStore()} />);

    fireEvent.click(await screen.findByRole("button", { name: "Restart the Daemon" }));
    fireEvent.click(await screen.findByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Restart the Daemon" }));
    fireEvent.keyDown(await screen.findByRole("dialog"), { key: "Escape" });
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(commands).toEqual(["last_update"]);
  });

  it("restarts the Daemon once confirmed, and recovers on the answer that follows", async () => {
    const commands = rust(failed(mismatch(21)), { restart_daemon: () => undefined });
    render(<App store={createDaemonStore()} />);
    fireEvent.click(await screen.findByRole("button", { name: "Restart the Daemon" }));
    const sheet = await screen.findByRole("dialog");

    fireEvent.click(within(sheet).getByRole("button", { name: "Restart the Daemon" }));
    await act(async () => {
      await Promise.resolve();
    });
    expect(commands).toEqual(["last_update", "restart_daemon"]);

    await tick(answered());
    expect(await screen.findByRole("region", { name: "Overview" })).toBeInTheDocument();
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });

  it("shows the error of a stop that exits 125 and sends no start", async () => {
    const commands = rust(failed(mismatch(21)), {
      restart_daemon: () =>
        Promise.reject({
          kind: "daemon_stop",
          message: "procly: the Daemon (pid 4242) did not stop within 10 s",
        }),
    });
    render(<App store={createDaemonStore()} />);
    fireEvent.click(await screen.findByRole("button", { name: "Restart the Daemon" }));
    const sheet = await screen.findByRole("dialog");

    fireEvent.click(within(sheet).getByRole("button", { name: "Restart the Daemon" }));

    expect(
      await screen.findByText("procly: the Daemon (pid 4242) did not stop within 10 s"),
    ).toBeInTheDocument();
    expect(commands).toEqual(["last_update", "restart_daemon"]);
    expect(commands).not.toContain("start_daemon");
  });
});
