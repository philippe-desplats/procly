import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { afterEach, describe, expect, it } from "vitest";

import { App } from "./App";
import { createDaemonStore, type Update } from "./daemon";
import type { Response } from "./protocol/Response";

const AT = Date.UTC(2026, 10, 5, 19, 35, 0);
const PATH = "/p/work/acme/web-fix";

/**
 * A Worktree holding every kind of line the screen draws: Services (one of them a Group row, which the protocol does
 * not tell from the others, and one that ended), an Isolated Compose service, 2 Foreign processes and 2 Jobs. A
 * Service a Group covers has no row of its own, since only the Group is spawned.
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
              end: { kind: "exited", code: 3 },
            },
            {
              service: "dev",
              port: 18733,
              reattached: false,
              started_at: AT - 30_000,
              restartable: true,
              state: "healthy",
            },
          ],
          jobs: [
            { id: "ea79fa81", command: "sleep 61", cwd: PATH, state: "running" },
            {
              id: "a4e2ea77",
              command: "sh -c 'exit 3'",
              cwd: PATH,
              state: "ended",
              end: { kind: "exited", code: 3 },
            },
          ],
          foreign: [
            { kind: "process", pid: 4242, command: "node server.js", ports: [3000] },
            { kind: "container", name: "legacy-db", project: "legacy", service: "db", ports: [5432] },
          ],
          containers: [{ service: "cache", project: "web_fix", compose: "redis", port: 18740 }],
        },
      ],
    },
  ],
  jobs_outside_repositories: [
    { id: "c0ffee01", command: "make watch", cwd: "/p/tmp/scratch", state: "running" },
  ],
};

interface Call {
  command: string;
  args: unknown;
}

/** Plays the Rust side: `answers` says what each action returns, every command the window sends is kept. */
function app(answers: Record<string, () => unknown> = {}, response: Response = scene): Call[] {
  const calls: Call[] = [];
  const update: Update = { at_ms: AT, response, needs_you: [], seen: [] };
  mockIPC(
    (command, args) => {
      calls.push({ command, args });
      if (command === "last_update") {
        return update;
      }
      if (command === "refresh" || command === "stop_follow") {
        return undefined;
      }
      // The inspector follows what is picked: it waits for output that never comes, and reads a Compose service once.
      if (command === "follow_logs") {
        return new Promise<void>(() => undefined);
      }
      if (command === "service_logs") {
        return { response: "logs", output: "" };
      }
      const answer = answers[command];
      return answer === undefined ? Promise.reject(new Error(`unexpected command ${command}`)) : answer();
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

function inspector(): HTMLElement {
  return screen.getByRole("complementary", { name: "Inspector" });
}

/** Picks the row of `center` that starts with `name`, which puts it in the inspector with its actions. */
function pick(center: HTMLElement, name: string): HTMLElement {
  fireEvent.click(within(center).getByRole("button", { name: new RegExp(`^${name}`) }));
  return inspector();
}

function written(): string[] {
  const texts: string[] = [];
  Object.defineProperty(navigator, "clipboard", {
    configurable: true,
    value: {
      writeText: (text: string) => {
        texts.push(text);
        return Promise.resolve();
      },
    },
  });
  return texts;
}

const down = (): unknown => ({ response: "down", stopped: ["web"], released: [] });
const up = (): unknown => ({
  response: "up",
  service: "web",
  outcome: "started",
  port: 18731,
  url: null,
  ports: {},
  via: null,
  needs: [],
  urls: {},
  warnings: [],
});

describe("the Worktree screen", () => {
  afterEach(() => {
    cleanup();
    clearMocks();
  });

  it("lists what runs in the Worktree: Services, Isolated Compose services, Foreign processes and Jobs", async () => {
    app();

    const center = await openWorktree();

    expect(within(center).getByRole("heading", { name: "Foreign processes" })).toBeInTheDocument();
    expect(within(center).getByText("node server.js")).toBeInTheDocument();
    expect(within(center).getByText("legacy-db")).toBeInTheDocument();
    expect(within(center).getByRole("heading", { name: "Isolated Compose services" })).toBeInTheDocument();
    expect(within(center).getByText("cache")).toBeInTheDocument();
    expect(within(center).getByRole("heading", { name: "Jobs" })).toBeInTheDocument();
    expect(within(center).getByText("sleep 61")).toBeInTheDocument();
    for (const service of ["web", "api", "old", "dev"]) {
      expect(within(center).getByText(service)).toBeInTheDocument();
    }
  });

  it("draws no Lease and no Logical resource, only what the protocol returns", async () => {
    app();

    const center = await openWorktree();

    expect(within(center).queryByText(/lease/i)).not.toBeInTheDocument();
    expect(within(center).queryByText(/logical/i)).not.toBeInTheDocument();
  });

  it("offers no section for what the Worktree does not have", async () => {
    const bare: Response = {
      ...(scene as Extract<Response, { response: "status" }>),
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
                  started_at: AT,
                  restartable: true,
                  state: "healthy",
                },
              ],
              jobs: [],
            },
          ],
        },
      ],
    };
    app({}, bare);

    const center = await openWorktree();

    expect(within(center).queryByRole("heading", { name: "Foreign processes" })).not.toBeInTheDocument();
    expect(
      within(center).queryByRole("heading", { name: "Isolated Compose services" }),
    ).not.toBeInTheDocument();
    expect(within(center).queryByRole("heading", { name: "Jobs" })).not.toBeInTheDocument();
  });

  it("stops a running Service with the Worktree as directory, with no confirmation sheet", async () => {
    const calls = app({ stop_service: down });

    const center = await openWorktree();
    fireEvent.click(within(pick(center, "web")).getByRole("button", { name: "Stop web" }));

    await waitFor(() => {
      expect(calls.filter((call) => call.command === "stop_service")).toEqual([
        { command: "stop_service", args: { service: "web", cwd: PATH } },
      ]);
    });
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument();
    // The next tick of the poller shows the effect, and Refresh asks for it at once.
    await waitFor(() => {
      expect(calls.map((call) => call.command)).toContain("refresh");
    });
  });

  it("stops an Isolated Compose service like a Service", async () => {
    const calls = app({ stop_service: down });

    const center = await openWorktree();
    fireEvent.click(within(pick(center, "cache")).getByRole("button", { name: "Stop cache" }));

    await waitFor(() => {
      expect(calls).toContainEqual({ command: "stop_service", args: { service: "cache", cwd: PATH } });
    });
  });

  it("never draws a Stop in red, and has no Stop All and no Stop of a Job", async () => {
    app();

    const center = await openWorktree();

    // The center only picks, the inspector holds the actions of what is picked.
    expect(within(center).queryByRole("button", { name: /^Stop / })).not.toBeInTheDocument();
    const stop = within(pick(center, "web")).getByRole("button", { name: "Stop web" });
    expect(stop.className).not.toMatch(/danger|destructive|fail|red/);
    expect(screen.queryByRole("button", { name: /stop all/i })).not.toBeInTheDocument();
    const stopsOf = (row: string): (string | null)[] =>
      within(pick(center, row))
        .queryAllByRole("button", { name: /^Stop / })
        .map((button) => button.getAttribute("aria-label"));
    expect(stopsOf("api")).toEqual(["Stop api"]);
    expect(stopsOf("dev")).toEqual(["Stop dev"]);
    expect(stopsOf("cache")).toEqual(["Stop cache"]);
    // `old` ended, so it has nothing to stop, and a Job has no Stop at all.
    expect(stopsOf("old")).toEqual([]);
    fireEvent.click(within(center).getByRole("button", { name: /sleep 61/ }));
    expect(within(inspector()).queryByRole("button", { name: /^Stop / })).not.toBeInTheDocument();
  });

  it("restarts a Service whose environment the Daemon holds", async () => {
    const calls = app({ restart_service: up });

    const center = await openWorktree();
    fireEvent.click(within(pick(center, "web")).getByRole("button", { name: "Restart web" }));

    await waitFor(() => {
      expect(calls).toContainEqual({ command: "restart_service", args: { service: "web", cwd: PATH } });
    });
    await waitFor(() => {
      expect(calls.map((call) => call.command)).toContain("refresh");
    });
  });

  it("restarts a Service that ended, since the Daemon keeps its environment", async () => {
    app();

    const center = await openWorktree();
    const panel = within(pick(center, "old"));

    expect(panel.getByRole("button", { name: "Restart old" })).toBeEnabled();
    expect(panel.queryByRole("button", { name: "Stop old" })).not.toBeInTheDocument();
  });

  it("offers the command to copy, and no Restart, for a Service the Daemon holds no environment for", async () => {
    const calls = app();

    const center = await openWorktree();
    const panel = within(pick(center, "api"));

    expect(panel.queryByRole("button", { name: "Restart api" })).not.toBeInTheDocument();
    expect(panel.getByText(`(cd ${PATH} && procly restart api)`)).toBeInTheDocument();
    expect(calls.map((call) => call.command)).not.toContain("restart_service");
  });

  it("offers the command to copy for an Isolated Compose service, whose environment the status does not tell", async () => {
    app();

    const center = await openWorktree();
    const panel = within(pick(center, "cache"));

    expect(panel.queryByRole("button", { name: "Restart cache" })).not.toBeInTheDocument();
    expect(panel.getByText(`(cd ${PATH} && procly restart cache)`)).toBeInTheDocument();
  });

  it("puts Stop and Restart on the row of a Group, the only row a Group's Services have", async () => {
    app();

    const center = await openWorktree();
    const panel = within(pick(center, "dev"));

    // The Group runs one process for the Services it covers, and `down` refuses a covered Service with 125, so the
    // status holds a row for the Group and none for what it covers.
    expect(panel.getByRole("button", { name: "Stop dev" })).toBeEnabled();
    expect(panel.getByRole("button", { name: "Restart dev" })).toBeEnabled();
    expect(within(center).queryByText("frontend")).not.toBeInTheDocument();
  });

  it("copies exactly the text a copy box shows", async () => {
    app();
    const texts = written();

    const center = await openWorktree();
    const box = within(pick(center, "api"))
      .getByText(`(cd ${PATH} && procly restart api)`)
      .closest(".copybox");
    expect(box).not.toBeNull();
    fireEvent.click(within(box as HTMLElement).getByRole("button", { name: /^Copy/ }));

    expect(texts).toEqual([`(cd ${PATH} && procly restart api)`]);
  });

  it("quotes a path the shell would split, in what it shows and in what it copies", async () => {
    const spaced: Response = JSON.parse(
      JSON.stringify(scene).replaceAll(PATH, "/p/work/acme/web fix"),
    ) as Response;
    app({}, spaced);
    const texts = written();

    render(<App store={createDaemonStore()} />);
    fireEvent.click(await screen.findByRole("button", { name: /^web-fix/ }));
    const center = screen.getByRole("main");
    await within(center).findByRole("heading", { name: "Services" });
    const shown = `(cd '/p/work/acme/web fix' && procly restart api)`;
    const box = (await within(pick(center, "api")).findByText(shown)).closest(".copybox");
    fireEvent.click(within(box as HTMLElement).getByRole("button", { name: /^Copy/ }));

    expect(texts).toEqual([shown]);
  });

  it("shows why a Restart failed, next to the Service, and sends no Up", async () => {
    const calls = app({
      restart_service: () => ({ response: "error", message: "web has no environment to restart with" }),
    });

    const center = await openWorktree();
    const panel = within(pick(center, "web"));
    fireEvent.click(panel.getByRole("button", { name: "Restart web" }));

    expect(await panel.findByRole("alert")).toHaveTextContent("web has no environment to restart with");
    expect(calls.map((call) => call.command)).not.toContain("up");
  });

  it("shows a Service that did not become healthy after a Restart", async () => {
    app({
      restart_service: () => ({
        response: "unhealthy",
        service: "web",
        outcome: "started",
        end: { kind: "exited", code: 1 },
        output: "boom",
        needed_by: null,
      }),
    });

    const center = await openWorktree();
    const panel = within(pick(center, "web"));
    fireEvent.click(panel.getByRole("button", { name: "Restart web" }));

    expect(await panel.findByRole("alert")).toHaveTextContent("web did not become healthy: exited 1");
  });

  it("shows a command that could not reach the Daemon", async () => {
    app({ stop_service: () => Promise.reject({ kind: "unreachable", message: "no Daemon answered" }) });

    const center = await openWorktree();
    const panel = within(pick(center, "web"));
    fireEvent.click(panel.getByRole("button", { name: "Stop web" }));

    expect(await panel.findByRole("alert")).toHaveTextContent("no Daemon answered");
  });
});

describe("the Outside Repositories screen", () => {
  afterEach(() => {
    cleanup();
    clearMocks();
  });

  it("lists the Jobs started outside any Repository, apart from the Worktrees", async () => {
    app();
    render(<App store={createDaemonStore()} />);

    fireEvent.click(await screen.findByRole("button", { name: /^Outside Repositories/ }));

    const center = screen.getByRole("main");
    expect(await within(center).findByRole("heading", { name: "Outside Repositories" })).toBeInTheDocument();
    expect(within(center).getByText("make watch")).toBeInTheDocument();
    expect(within(center).getByText("/p/tmp/scratch")).toBeInTheDocument();
    expect(within(center).queryByRole("button", { name: /stop/i })).not.toBeInTheDocument();
  });

  it("says so when no Job started outside a Repository", async () => {
    app({}, { ...(scene as Extract<Response, { response: "status" }>), jobs_outside_repositories: [] });
    render(<App store={createDaemonStore()} />);

    fireEvent.click(await screen.findByRole("button", { name: /^Outside Repositories/ }));

    expect(await screen.findByText("No Job started outside a Repository.")).toBeInTheDocument();
  });
});
