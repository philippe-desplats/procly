import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import type { Channel } from "@tauri-apps/api/core";
import { emit } from "@tauri-apps/api/event";
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { afterEach, describe, expect, it } from "vitest";

import { App } from "./App";
import { createDaemonStore, type Update } from "./daemon";
import recorded from "./fixtures/status.json";
import type { Response } from "./protocol/Response";
import type { StatusReport } from "./protocol/StatusReport";

const AT = Date.UTC(2026, 10, 5, 19, 35, 0);

/** A `Status { all: true }` answer recorded from a running Daemon, its paths replaced by neutral ones. */
const status = recorded as unknown as StatusReport & { response: "status" };

function answered(report: StatusReport & { response: "status" }, at = AT): Update {
  return { at_ms: at, response: report, needs_you: [], seen: [] };
}

/** The recorded answer with a Compose service in the main Worktree, which `Status` lists in `containers`. */
function withCompose(): StatusReport & { response: "status" } {
  const copy = structuredClone(status);
  const main = copy.repositories[0]?.worktrees[0];
  if (main === undefined) {
    throw new Error("the recorded answer has a main Worktree");
  }
  main.containers = [{ service: "db", project: "web-main", compose: "postgres", port: 24002 }];
  return copy;
}

/** A command the window sent to the Rust side. */
interface Sent {
  command: string;
  args: Record<string, unknown>;
}

/** The Rust side of the inspector: what the window asked for, and the channels it would write to. */
class Rust {
  readonly sent: Sent[] = [];
  readonly follows: {
    ticket: number;
    name: string;
    cwd: string;
    channel: Channel<Response>;
    end: () => void;
  }[] = [];
  snapshot: Response = { response: "logs", output: "" };

  constructor(last: Update) {
    mockIPC(
      (command, payload) => {
        const args = (payload ?? {}) as Record<string, unknown>;
        this.sent.push({ command, args });
        switch (command) {
          case "last_update":
            return last;
          case "refresh":
          case "stop_follow":
            return undefined;
          case "follow_logs":
            return new Promise<void>((resolve) => {
              this.follows.push({
                ticket: args["ticket"] as number,
                name: args["name"] as string,
                cwd: args["cwd"] as string,
                channel: args["onOutput"] as Channel<Response>,
                end: resolve,
              });
            });
          case "service_logs":
            return this.snapshot;
          default:
            return Promise.reject(new Error(`unexpected command ${command}`));
        }
      },
      { shouldMockEvents: true },
    );
  }

  count(command: string): number {
    return this.sent.filter((sent) => sent.command === command).length;
  }

  last(command: string): Sent | undefined {
    return this.sent.filter((sent) => sent.command === command).at(-1);
  }

  /** The follow that runs now, the newest. */
  get follow() {
    const follow = this.follows.at(-1);
    if (follow === undefined) {
      throw new Error("no follow was asked for");
    }
    return follow;
  }

  async write(response: Response, follow = this.follow): Promise<void> {
    await act(async () => {
      follow.channel.onmessage(response);
      await Promise.resolve();
    });
  }
}

async function open(rust: Rust): Promise<void> {
  render(<App store={createDaemonStore()} />);
  await screen.findByRole("heading", { name: "Overview" });
  expect(rust.count("last_update")).toBe(1);
}

const logs = (output: string): Response => ({ response: "logs", output });
const output = (text: string): Response => ({ response: "output", text });

function inspector(): HTMLElement {
  return screen.getByRole("complementary", { name: "Inspector" });
}

/** The text of the output pane. */
function shown(): string {
  return within(inspector()).getByRole("log", { name: "Output" }).textContent;
}

afterEach(() => {
  cleanup();
  clearMocks();
});

describe("the inspector", () => {
  it("states a Service in words with what to do next, and follows its output", async () => {
    const rust = new Rust(answered(status));
    await open(rust);

    fireEvent.click(screen.getByRole("button", { name: /^api/ }));

    const panel = within(inspector());
    expect(await panel.findByText("Running and healthy.")).toBeInTheDocument();
    expect(panel.getByText("Nothing to do.")).toBeInTheDocument();
    expect(panel.getByText(":18731")).toBeInTheDocument();
    expect(panel.getByText(/Started/)).toHaveTextContent("Started");
    await waitFor(() => {
      expect(rust.count("follow_logs")).toBe(1);
    });
    expect(rust.follow).toMatchObject({ name: "api", cwd: "/p/work/acme/web" });
  });

  it("states a Job in words", async () => {
    const rust = new Rust(answered(status));
    await open(rust);
    fireEvent.click(screen.getByRole("button", { name: "web-fix" }));

    fireEvent.click(await screen.findByRole("button", { name: /sh -c 'exit 3'/ }));

    const panel = within(inspector());
    expect(await panel.findByText("Exited with code 3.")).toBeInTheDocument();
    expect(panel.getByText("Read the end of its output to see what failed.")).toBeInTheDocument();
    await waitFor(() => {
      expect(rust.follow).toMatchObject({ name: "a4e2ea77", cwd: "/p/work/acme/web-fix" });
    });
  });

  it("shows the backlog, then each line as it arrives, then the end", async () => {
    const rust = new Rust(answered(status));
    await open(rust);
    fireEvent.click(screen.getByRole("button", { name: /^api/ }));
    await waitFor(() => {
      expect(rust.follows).toHaveLength(1);
    });

    await rust.write(logs("booting\n"));
    await waitFor(() => {
      expect(shown()).toBe("booting\n");
    });
    await rust.write(output("ready\n"));
    await waitFor(() => {
      expect(shown()).toBe("booting\nready\n");
    });
    expect(within(inspector()).getByRole("status")).toHaveTextContent("Following");

    await rust.write({ response: "output_dropped", bytes: 2048 });
    await rust.write({ response: "logs_ended" });
    await waitFor(() => {
      expect(within(inspector()).getByRole("status")).toHaveTextContent("Ended");
    });
    expect(shown()).toContain("2048 bytes of output were skipped");
  });

  it("draws colours and bold in the palette and never the escape characters", async () => {
    const rust = new Rust(answered(status));
    await open(rust);
    fireEvent.click(screen.getByRole("button", { name: /^api/ }));
    await waitFor(() => {
      expect(rust.follows).toHaveLength(1);
    });

    await rust.write(logs("\x1b[1;31mfatal\x1b[0m: \x1b[2Kport in use\n"));

    await waitFor(() => {
      expect(shown()).toBe("fatal: port in use\n");
    });
    const fatal = within(inspector()).getByText("fatal");
    expect(fatal).toHaveClass("fg-red", "bold");
  });

  it("keeps following when the poller reports the Service again, even restarted", async () => {
    const rust = new Rust(answered(status));
    await open(rust);
    fireEvent.click(screen.getByRole("button", { name: /^api/ }));
    await waitFor(() => {
      expect(rust.follows).toHaveLength(1);
    });
    await rust.write(logs("booting\n"));
    await waitFor(() => {
      expect(shown()).toBe("booting\n");
    });

    const restarted = structuredClone(status);
    const service = restarted.repositories[0]?.worktrees[0]?.services[0];
    if (service === undefined) {
      throw new Error("the recorded answer has a Service");
    }
    service.started_at += 5000;
    await act(async () => {
      await emit("daemon-update", answered(restarted, AT + 2000));
    });

    expect(rust.follows).toHaveLength(1);
    expect(rust.count("stop_follow")).toBe(0);
    expect(shown()).toBe("booting\n");
  });

  it("follows one item at a time: another selection closes the first and starts from a fresh backlog", async () => {
    const rust = new Rust(answered(status));
    await open(rust);
    fireEvent.click(screen.getByRole("button", { name: /^api/ }));
    await waitFor(() => {
      expect(rust.follows).toHaveLength(1);
    });
    const first = rust.follow;
    await rust.write(logs("from the first\n"), first);

    fireEvent.click(screen.getByRole("button", { name: "web-fix" }));
    fireEvent.click(await screen.findByRole("button", { name: /sleep 61/ }));

    await waitFor(() => {
      expect(rust.follows).toHaveLength(2);
    });
    const stop = rust.last("stop_follow");
    expect(stop?.args["ticket"]).toBeGreaterThan(first.ticket);
    expect(rust.follow.ticket).toBeGreaterThan(stop?.args["ticket"] as number);
    expect(rust.follow).toMatchObject({ name: "9d3a1f1b", cwd: "/p/work/acme/web-fix" });
    expect(shown()).toBe("");
    // What the first follow still writes is no longer shown.
    await rust.write(output("late line of the first\n"), first);
    expect(shown()).toBe("");
    await rust.write(logs("from the second\n"));
    await waitFor(() => {
      expect(shown()).toBe("from the second\n");
    });
  });

  it("closes the follow when the selection leaves the screen", async () => {
    const rust = new Rust(answered(status));
    await open(rust);
    fireEvent.click(screen.getByRole("button", { name: /^api/ }));
    await waitFor(() => {
      expect(rust.follows).toHaveLength(1);
    });

    fireEvent.click(screen.getByRole("button", { name: /^Settings/ }));

    await waitFor(() => {
      expect(rust.count("stop_follow")).toBe(1);
    });
    expect(rust.last("stop_follow")?.args["ticket"]).toBeGreaterThan(rust.follow.ticket);
  });

  it("pauses when the window is hidden and starts again from a fresh backlog when it comes back", async () => {
    const rust = new Rust(answered(status));
    await open(rust);
    fireEvent.click(screen.getByRole("button", { name: /^api/ }));
    await waitFor(() => {
      expect(rust.follows).toHaveLength(1);
    });
    await rust.write(logs("before the hide\n"));

    // Rust closed the connection when the window was hidden, so the command comes back.
    await act(async () => {
      await emit("window-visible", false);
      rust.follow.end();
    });
    await waitFor(() => {
      expect(within(inspector()).getByRole("status")).toHaveTextContent("Paused");
    });
    expect(rust.follows).toHaveLength(1);

    await act(async () => {
      await emit("window-visible", true);
    });

    await waitFor(() => {
      expect(rust.follows).toHaveLength(2);
    });
    expect(rust.follow.ticket).toBeGreaterThan(rust.follows[0]?.ticket ?? Infinity);
    expect(shown()).toBe("");
    await rust.write(logs("after the return\n"));
    await waitFor(() => {
      expect(shown()).toBe("after the return\n");
    });
  });

  it("follows a Service again once it runs again after it ended", async () => {
    const ended = structuredClone(status);
    const service = ended.repositories[0]?.worktrees[0]?.services[0];
    if (service === undefined) {
      throw new Error("the recorded answer has a Service");
    }
    Object.assign(service, { state: "ended", end: { kind: "stopped" } });
    const rust = new Rust(answered(ended));
    await open(rust);
    fireEvent.click(screen.getByRole("button", { name: /^api/ }));
    await waitFor(() => {
      expect(rust.follows).toHaveLength(1);
    });
    await rust.write(logs("last words\n"));
    await rust.write({ response: "logs_ended" });
    expect(within(inspector()).getByText("Stopped on request.")).toBeInTheDocument();

    await act(async () => {
      await emit("daemon-update", answered(status, AT + 2000));
    });

    await waitFor(() => {
      expect(rust.follows).toHaveLength(2);
    });
    expect(shown()).toBe("");
  });

  it("shows the reason when the Daemon refuses to follow", async () => {
    const rust = new Rust(answered(status));
    await open(rust);
    fireEvent.click(screen.getByRole("button", { name: /^api/ }));
    await waitFor(() => {
      expect(rust.follows).toHaveLength(1);
    });

    await rust.write({ response: "error", message: "no job or service api" });

    await waitFor(() => {
      expect(within(inspector()).getByRole("alert")).toHaveTextContent("no job or service api");
    });
  });
});

describe("the inspector on a Compose service", () => {
  async function openCompose(rust: Rust): Promise<void> {
    await open(rust);
    fireEvent.click(screen.getByRole("button", { name: /^main\s*main$/ }));
    fireEvent.click(await screen.findByRole("button", { name: /^db/ }));
  }

  it("shows a snapshot with its time and never follows", async () => {
    const rust = new Rust(answered(withCompose()));
    rust.snapshot = logs("database system is ready\n");
    await openCompose(rust);

    const panel = within(inspector());
    expect(await panel.findByText(/database system is ready/)).toBeInTheDocument();
    expect(rust.count("follow_logs")).toBe(0);
    expect(rust.last("service_logs")?.args).toEqual({ name: "db", cwd: "/p/work/acme/web" });
    expect(panel.getByText(/^Snapshot at \d\d:\d\d:\d\d$/)).toBeInTheDocument();
    expect(
      panel.getByText("Running as the Compose service postgres of the project web-main."),
    ).toBeInTheDocument();
  });

  it("reads the output again on Refresh", async () => {
    const rust = new Rust(answered(withCompose()));
    rust.snapshot = logs("first\n");
    await openCompose(rust);
    await within(inspector()).findByText(/first/);

    rust.snapshot = logs("first\nsecond\n");
    fireEvent.click(within(inspector()).getByRole("button", { name: "Refresh" }));

    await waitFor(() => {
      expect(shown()).toBe("first\nsecond\n");
    });
    expect(rust.count("service_logs")).toBe(2);
  });

  it("shows the error and the command to copy when docker compose fails", async () => {
    const rust = new Rust(answered(withCompose()));
    rust.snapshot = { response: "error", message: "docker: command not found" };
    await openCompose(rust);

    const panel = within(inspector());
    expect(await panel.findByRole("alert")).toHaveTextContent("docker: command not found");
    expect(panel.getByText("(cd /p/work/acme/web && procly logs db)")).toBeInTheDocument();
  });

  it("copies exactly the command it shows", async () => {
    const rust = new Rust(answered(withCompose()));
    rust.snapshot = { response: "error", message: "docker: command not found" };
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
    await openCompose(rust);
    const panel = within(inspector());
    await panel.findByRole("alert");

    fireEvent.click(panel.getByRole("button", { name: "Copy" }));

    expect(written).toEqual(["(cd /p/work/acme/web && procly logs db)"]);
  });
});
