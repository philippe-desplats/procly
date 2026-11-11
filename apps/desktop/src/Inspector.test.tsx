import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import type { Channel } from "@tauri-apps/api/core";
import { emit } from "@tauri-apps/api/event";
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { afterEach, describe, expect, it, vi } from "vitest";

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
  /** What `open_localhost` rejects with, nothing when it opens. */
  refusal: unknown = null;

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
          case "open_localhost":
            return this.refusal === null ? undefined : Promise.reject(this.refusal);
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
    expect(panel.getByText("18731")).toHaveClass("port");
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
    fireEvent.click(screen.getByRole("button", { name: /^main\s*Main worktree$/ }));
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

    // The panel holds another copy box, the command to restart the Compose service, so the one of the output is named.
    fireEvent.click(
      within(panel.getByRole("region", { name: "Output" })).getByRole("button", { name: "Copy" }),
    );

    expect(written).toEqual(["(cd /p/work/acme/web && procly logs db)"]);
  });
});

/** Stands for the clipboard: what the window copies is kept. */
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

/** Presses Find, which reveals the field, and answers the field. */
function revealFind(): HTMLElement {
  fireEvent.click(within(inspector()).getByRole("button", { name: "Find" }));
  return within(inspector()).getByRole("searchbox", { name: "Find in output" });
}

describe("the inspector of a Job", () => {
  it("says under the command in which Worktree the Job runs, or that it runs outside Repositories", async () => {
    const outside = structuredClone(status);
    outside.jobs_outside_repositories = [
      { id: "c0ffee01", command: "sleep 5", cwd: "/p/work/tmp", state: "running" },
    ];
    const rust = new Rust(answered(status));
    await open(rust);
    fireEvent.click(screen.getByRole("button", { name: "web-fix" }));
    fireEvent.click(await screen.findByRole("button", { name: /sh -c 'exit 3'/ }));

    const heading = await within(inspector()).findByRole("heading", { name: "sh -c 'exit 3'" });
    expect(heading.nextElementSibling).toHaveTextContent("Job in web-fix");
    cleanup();
    clearMocks();

    const other = new Rust(answered(outside));
    await open(other);
    fireEvent.click(screen.getByRole("button", { name: /^\d+ Jobs?$/ }));
    fireEvent.click(await screen.findByRole("button", { name: /sleep 5/ }));

    const outsideHeading = await within(inspector()).findByRole("heading", { name: "sleep 5" });
    expect(outsideHeading.nextElementSibling).toHaveTextContent("Job outside Repositories");
  });

  it("gives the command to wait for it from a terminal, in a copy box that copies exactly that", async () => {
    const rust = new Rust(answered(status));
    const written = clipboard();
    await open(rust);
    fireEvent.click(screen.getByRole("button", { name: "web-fix" }));
    fireEvent.click(await screen.findByRole("button", { name: /sh -c 'exit 3'/ }));

    const panel = within(inspector());
    expect(await panel.findByText("Wait for it from a terminal")).toBeInTheDocument();
    const box = panel.getByText("procly wait a4e2ea77").closest(".copybox");
    expect(box).not.toBeNull();
    fireEvent.click(within(box as HTMLElement).getByRole("button", { name: "Copy" }));

    expect(written).toEqual(["procly wait a4e2ea77"]);
  });

  it("gives it for a Job that runs too, and none for a Service", async () => {
    const rust = new Rust(answered(status));
    await open(rust);
    fireEvent.click(screen.getByRole("button", { name: /^api/ }));
    await within(inspector()).findByRole("heading", { name: "api" });
    expect(within(inspector()).queryByText(/procly wait/)).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: /^main/ }));
    fireEvent.click(await screen.findByRole("button", { name: /sleep 60/ }));

    expect(await within(inspector()).findByText("procly wait ea79fa81")).toBeInTheDocument();
  });
});

describe("the inspector of a Service", () => {
  it("says under the name that it is a Process service, and in which Repository", async () => {
    const rust = new Rust(answered(status));
    await open(rust);

    fireEvent.click(screen.getByRole("button", { name: /^api/ }));

    const panel = within(inspector());
    const name = await panel.findByRole("heading", { name: "api" });
    expect(panel.getByText("Process service in web")).toBeInTheDocument();
    expect(name.nextElementSibling).toHaveTextContent("Process service in web");
  });

  it("offers to open localhost on the port of a Service that runs, and asks Rust to, with the port alone", async () => {
    const rust = new Rust(answered(status));
    await open(rust);
    fireEvent.click(screen.getByRole("button", { name: /^api/ }));

    fireEvent.click(await within(inspector()).findByRole("button", { name: "Open localhost:18731" }));

    await waitFor(() => {
      expect(rust.last("open_localhost")?.args).toEqual({ port: 18731 });
    });
  });

  it("draws the port as a pill with Open localhost beside it on the same line, and no Browser row", async () => {
    const rust = new Rust(answered(status));
    await open(rust);
    fireEvent.click(screen.getByRole("button", { name: /^api/ }));

    const button = await within(inspector()).findByRole("button", { name: "Open localhost:18731" });
    const pill = within(inspector()).getByText("18731");

    expect(pill).toHaveClass("port");
    expect(pill.parentElement).toContainElement(button);
    expect(within(inspector()).queryByText("Browser")).not.toBeInTheDocument();
  });

  it("keeps the field of Find away until Find is pressed, and pressing Find again closes it and clears the filter", async () => {
    const rust = new Rust(answered(status));
    await open(rust);
    fireEvent.click(screen.getByRole("button", { name: /^api/ }));
    await waitFor(() => {
      expect(rust.follows).toHaveLength(1);
    });
    await rust.write(logs("booting\nready\n"));
    expect(within(inspector()).queryByRole("searchbox")).not.toBeInTheDocument();

    const field = revealFind();
    expect(field).toHaveFocus();
    fireEvent.change(field, { target: { value: "ready" } });
    await waitFor(() => {
      expect(shown()).not.toContain("booting");
    });
    fireEvent.click(within(inspector()).getByRole("button", { name: "Find" }));

    expect(within(inspector()).queryByRole("searchbox")).not.toBeInTheDocument();
    await waitFor(() => {
      expect(shown()).toContain("booting");
    });
  });

  it("keeps the live follow and its caption beside Find and Wrap", async () => {
    const rust = new Rust(answered(status));
    await open(rust);
    fireEvent.click(screen.getByRole("button", { name: /^api/ }));
    await waitFor(() => {
      expect(rust.follows).toHaveLength(1);
    });
    await rust.write(logs("booting\n"));

    const status_ = within(inspector()).getByRole("status");
    await waitFor(() => {
      expect(status_).toHaveTextContent("Following");
    });
    expect(status_.parentElement).toContainElement(within(inspector()).getByRole("button", { name: "Find" }));
    expect(status_.parentElement).toContainElement(within(inspector()).getByRole("button", { name: "Wrap" }));
  });

  it("gives the output the room that is left of the inspector", async () => {
    const rust = new Rust(answered(status));
    await open(rust);
    fireEvent.click(screen.getByRole("button", { name: /^api/ }));

    const output = await within(inspector()).findByRole("region", { name: "Output" });

    expect(output).toHaveClass("output-pane");
    expect(output.parentElement).toHaveClass("inspector-panel");
  });

  it("offers no localhost for a Service without a port, nor for one that ended", async () => {
    const withoutPort = structuredClone(status);
    const api = withoutPort.repositories[0]?.worktrees[0]?.services[0];
    const ended = structuredClone(status);
    const endedApi = ended.repositories[0]?.worktrees[0]?.services[0];
    if (api === undefined || endedApi === undefined) {
      throw new Error("the recorded answer has the api Service");
    }
    api.port = null;
    Object.assign(endedApi, { state: "ended", end: { kind: "exited", code: 1 } });

    for (const report of [withoutPort, ended]) {
      const rust = new Rust(answered(report));
      await open(rust);
      fireEvent.click(screen.getByRole("button", { name: /^api/ }));
      await within(inspector()).findByRole("heading", { name: "api" });

      expect(within(inspector()).queryByRole("button", { name: /^Open localhost/ })).not.toBeInTheDocument();
      cleanup();
      clearMocks();
    }
  });

  it("finds lines of the output: Find keeps the lines that hold the text, whatever its case, and clearing it brings the rest back", async () => {
    const rust = new Rust(answered(status));
    await open(rust);
    fireEvent.click(screen.getByRole("button", { name: /^api/ }));
    await waitFor(() => {
      expect(rust.follows).toHaveLength(1);
    });
    await rust.write(logs("booting\nReady on 18731\nGET /\nready again\n"));
    await waitFor(() => {
      expect(shown()).toContain("booting");
    });

    const find = revealFind();
    fireEvent.change(find, { target: { value: "READY" } });

    await waitFor(() => {
      expect(shown()).not.toContain("booting");
    });
    expect(shown()).toContain("Ready on 18731");
    expect(shown()).toContain("ready again");
    expect(shown()).not.toContain("GET /");
    expect(within(inspector()).getByText("2 matching lines")).toBeInTheDocument();

    fireEvent.change(find, { target: { value: "" } });

    await waitFor(() => {
      expect(shown()).toContain("booting");
    });
    expect(shown()).toContain("GET /");
  });

  it("says so when no line holds the text", async () => {
    const rust = new Rust(answered(status));
    await open(rust);
    fireEvent.click(screen.getByRole("button", { name: /^api/ }));
    await waitFor(() => {
      expect(rust.follows).toHaveLength(1);
    });
    await rust.write(logs("booting\n"));

    fireEvent.change(revealFind(), { target: { value: "absent" } });

    await waitFor(() => {
      expect(within(inspector()).getByText("No line holds this text.")).toBeInTheDocument();
    });
  });

  it("keeps the lines as they come by default, and Wrap folds them, then No Wrap puts them back", async () => {
    const rust = new Rust(answered(status));
    await open(rust);
    fireEvent.click(screen.getByRole("button", { name: /^api/ }));
    await waitFor(() => {
      expect(rust.follows).toHaveLength(1);
    });
    const log = within(inspector()).getByRole("log", { name: "Output" });
    expect(log).not.toHaveClass("wrap");

    fireEvent.click(within(inspector()).getByRole("button", { name: "Wrap" }));

    expect(log).toHaveClass("wrap");
    expect(within(inspector()).queryByRole("button", { name: "Wrap" })).not.toBeInTheDocument();

    fireEvent.click(within(inspector()).getByRole("button", { name: "No Wrap" }));

    expect(log).not.toHaveClass("wrap");
    expect(within(inspector()).getByRole("button", { name: "Wrap" })).toBeInTheDocument();
  });

  describe("while it follows the end of the output", () => {
    /** The observers the window made, standing in for the ResizeObserver jsdom does not have. */
    const observers: { callback: ResizeObserverCallback; targets: Element[] }[] = [];

    class FakeResizeObserver {
      private readonly entry: { callback: ResizeObserverCallback; targets: Element[] };

      constructor(callback: ResizeObserverCallback) {
        this.entry = { callback, targets: [] };
        observers.push(this.entry);
      }

      observe(target: Element): void {
        this.entry.targets.push(target);
      }

      unobserve(target: Element): void {
        this.entry.targets = this.entry.targets.filter((one) => one !== target);
      }

      disconnect(): void {
        this.entry.targets = [];
      }
    }

    afterEach(() => {
      observers.length = 0;
      vi.unstubAllGlobals();
    });

    /** Tells the observers of `element` that its box changed size, as the window does once it is laid out again. */
    function resized(element: Element): void {
      act(() => {
        for (const observer of observers.filter((one) => one.targets.includes(element))) {
          observer.callback([], observer as unknown as ResizeObserver);
        }
      });
    }

    /**
     * Gives the output the sizes a laid-out window would, `content` tall in a box `box` tall, and keeps it scrolled
     * within them as a browser does. Returns how far it is from the end.
     */
    function laidOut(log: HTMLElement, size: { content: number; box: number }): () => number {
      let top = 0;
      Object.defineProperty(log, "scrollHeight", { configurable: true, get: () => size.content });
      Object.defineProperty(log, "clientHeight", { configurable: true, get: () => size.box });
      Object.defineProperty(log, "scrollTop", {
        configurable: true,
        get: () => top,
        set: (value: number) => {
          top = Math.max(0, Math.min(value, size.content - size.box));
        },
      });
      return () => size.content - top - size.box;
    }

    /** Opens the output of api, laid out 1000 px tall in a box 300 px tall, and scrolled to its end. */
    async function following(): Promise<{
      rust: Rust;
      log: HTMLElement;
      size: { content: number; box: number };
      fromEnd: () => number;
    }> {
      vi.stubGlobal("ResizeObserver", FakeResizeObserver);
      const rust = new Rust(answered(status));
      await open(rust);
      fireEvent.click(screen.getByRole("button", { name: /^api/ }));
      await waitFor(() => {
        expect(rust.follows).toHaveLength(1);
      });
      const log = within(inspector()).getByRole("log", { name: "Output" });
      const size = { content: 1000, box: 300 };
      const fromEnd = laidOut(log, size);
      await rust.write(logs("booting\nReady on 18731\n"));
      await waitFor(() => {
        expect(shown()).toContain("Ready on 18731");
      });
      expect(fromEnd()).toBe(0);
      return { rust, log, size, fromEnd };
    }

    it("follows the end again when Find opens and shrinks the box, though no line came", async () => {
      const { log, size, fromEnd } = await following();

      fireEvent.click(within(inspector()).getByRole("button", { name: "Find" }));
      size.box = 273;
      resized(log);

      expect(fromEnd()).toBe(0);
    });

    it("follows the end again when Wrap folds the lines and the output grows, though no line came", async () => {
      const { size, fromEnd } = await following();

      size.content = 19_000;
      fireEvent.click(within(inspector()).getByRole("button", { name: "Wrap" }));

      expect(fromEnd()).toBe(0);
    });

    it("leaves a reader who scrolled up where they are when the box changes size", async () => {
      const { log, size, fromEnd } = await following();
      log.scrollTop = 0;
      fireEvent.scroll(log);

      fireEvent.click(within(inspector()).getByRole("button", { name: "Find" }));
      size.box = 273;
      resized(log);

      expect(log.scrollTop).toBe(0);
      expect(fromEnd()).toBe(727);
    });
  });

  it("shows a refusal of Rust to open localhost under the button", async () => {
    const rust = new Rust(answered(status));
    await open(rust);
    fireEvent.click(screen.getByRole("button", { name: /^api/ }));
    const button = await within(inspector()).findByRole("button", { name: "Open localhost:18731" });
    rust.refusal = { kind: "launch", message: "cannot open the browser" };

    fireEvent.click(button);

    expect(await within(inspector()).findByRole("alert")).toHaveTextContent("cannot open the browser");
  });
});
