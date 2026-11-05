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

function failed(failure: Failure): Update {
  return { at_ms: AT, failure };
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

    expect(await screen.findByRole("heading", { name: "Daemon not running" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Start the Daemon" })).toBeEnabled();
    expect(screen.getByText("procly unit install")).toBeInTheDocument();
    expect(commands).toEqual(["last_update"]);
  });

  it("copies exactly the command it shows", async () => {
    rust(failed(NOT_RUNNING));
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
    expect(commands).toEqual(["last_update", "start_daemon"]);

    await tick({ at_ms: AT, response: status });
    expect(await screen.findByRole("region", { name: "Overview" })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "Daemon not running" })).not.toBeInTheDocument();
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
    rust({ at_ms: AT, response: status });
    render(<App store={createDaemonStore()} />);
    expect(await screen.findByRole("region", { name: "Overview" })).toBeInTheDocument();

    await tick(failed(NOT_RUNNING));
    expect(await screen.findByRole("heading", { name: "Daemon not running" })).toBeInTheDocument();

    await tick({ at_ms: AT + 5000, response: status });
    expect(await screen.findByRole("region", { name: "Overview" })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "Daemon not running" })).not.toBeInTheDocument();
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

    expect(await screen.findByRole("heading", { name: "Daemon of another version" })).toBeInTheDocument();
    expect(screen.getByText("The Daemon speaks protocol 21, this app speaks 22.")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Restart the Daemon" })).toBeEnabled();
  });

  it("asks for an update of the app on a newer Daemon, and offers no restart", async () => {
    rust(failed(mismatch(23)));

    render(<App store={createDaemonStore()} />);

    expect(await screen.findByRole("heading", { name: "Daemon of another version" })).toBeInTheDocument();
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

    await tick({ at_ms: AT, response: status });
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
