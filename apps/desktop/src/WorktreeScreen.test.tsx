import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { afterEach, describe, expect, it } from "vitest";

import { App } from "./App";
import { createDaemonStore, type Update } from "./daemon";
import { PortPill } from "./PortPill";
import type { Response } from "./protocol/Response";
import { partsOf } from "./test-rows";

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
            { kind: "process", pid: 4812, command: "node server.js", ports: [3000] },
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

  it("draws the glyph of each state in front of its row: a diamond, a ring, a dot", async () => {
    app();

    const center = await openWorktree();

    /** The glyph drawn on the row that holds `text`. */
    const glyphOn = (text: string): string | null | undefined =>
      within(center).getByText(text).closest("li")?.querySelector("svg")?.getAttribute("data-glyph");
    expect(glyphOn("node server.js")).toBe("foreign");
    expect(glyphOn("legacy-db")).toBe("foreign");
    expect(glyphOn("cache")).toBe("healthy");
    expect(glyphOn("sleep 61")).toBe("job");
  });

  it("opens every row with the glyph of its state, the word of the state following the name", async () => {
    app();

    const center = await openWorktree();

    const rows = within(center).getAllByRole("listitem");
    expect(rows.map(partsOf)).toEqual([
      {
        glyph: "foreign",
        name: "node server.js",
        word: "started outside Procly",
        where: undefined,
        ports: ["3000"],
      },
      {
        glyph: "foreign",
        name: "legacy-db",
        word: "started outside Procly",
        where: undefined,
        ports: ["5432"],
      },
      { glyph: "healthy", name: "web", word: "healthy", where: undefined, ports: ["18731"] },
      { glyph: "healthy", name: "api", word: "healthy", where: undefined, ports: ["18732"] },
      { glyph: "ended", name: "old", word: "exited 3", where: undefined, ports: [] },
      { glyph: "healthy", name: "dev", word: "healthy", where: undefined, ports: ["18733"] },
      { glyph: "healthy", name: "cache", word: "running", where: undefined, ports: ["18740"] },
      { glyph: "job", name: "sleep 61", word: "running", where: undefined, ports: [] },
      { glyph: "ended", name: "sh -c 'exit 3'", word: "exited 3", where: undefined, ports: [] },
    ]);
    for (const row of rows) {
      expect(row.querySelector(".item-select")?.firstElementChild?.textContent).toBe("");
    }
  });

  it("draws a Job of the Worktree as its glyph, then its command, then its state", async () => {
    app();
    const center = await openWorktree();

    for (const command of ["sleep 61", "sh -c 'exit 3'"]) {
      expect(inOrder(within(center).getByText(command).closest("li") as HTMLElement)).toEqual([
        "glyph",
        "command",
        "state",
      ]);
    }
  });

  it("draws a port as the same pill on a Worktree row as on an Overview row, and never as the text :port", async () => {
    app();
    const center = await openWorktree();

    const pill = within(center)
      .getAllByRole("listitem")
      .flatMap((row) => Array.from(row.querySelectorAll(".port")))
      .find((port) => port.textContent === "18731");
    // The one shared component draws it, so the row holds exactly the markup the component draws.
    const { container } = render(<PortPill port={18731} />);
    expect(pill?.outerHTML).toBe(container.innerHTML);
    expect(within(center).queryByText(/^:\d+$/)).not.toBeInTheDocument();
  });

  it("picks a Foreign process for the inspector, which gives the kill command to copy and no Stop", async () => {
    app();
    const texts = written();

    const center = await openWorktree();
    const row = within(center).getByText("node server.js").closest("button");
    expect(row).not.toBeNull();
    expect(row).toHaveAttribute("aria-pressed", "false");
    fireEvent.click(row as HTMLElement);

    expect(row).toHaveAttribute("aria-pressed", "true");
    const panel = within(inspector());
    expect(panel.getByRole("heading", { name: "node server.js" })).toBeInTheDocument();
    expect(panel.getByText("Foreign process in web-fix")).toBeInTheDocument();
    expect(panel.getByText("started outside Procly")).toBeInTheDocument();
    expect(panel.getByText(/Procly never stops a process it did not start/)).toBeInTheDocument();
    expect(panel.getByText("3000")).toHaveClass("port");
    expect(panel.getByText("Process").nextElementSibling).toHaveTextContent("node server.js, pid 4812");
    expect(panel.getByText("Stop it yourself")).toBeInTheDocument();
    expect(panel.queryByRole("button", { name: /^Stop/ })).not.toBeInTheDocument();
    expect(panel.queryByRole("button", { name: /^Restart/ })).not.toBeInTheDocument();

    const box = panel.getByText("kill 4812").closest(".copybox");
    expect(box).not.toBeNull();
    fireEvent.click(within(box as HTMLElement).getByRole("button", { name: /^Copy/ }));
    expect(texts).toEqual(["kill 4812"]);
  });

  it("lays the Foreign process inspector out as the mockup does: facts, then the sentence, then the copy box", async () => {
    app();

    const center = await openWorktree();
    fireEvent.click(within(center).getByText("node server.js").closest("button") as HTMLElement);

    const panel = within(inspector());
    const port = panel.getByText("Port");
    const process = panel.getByText("Process");
    const sentence = panel.getByText(/Procly never stops a process it did not start/);
    const label = panel.getByText("Stop it yourself");
    /** Whether `first` comes before `second` in the document. */
    const before = (first: HTMLElement, second: HTMLElement): boolean =>
      (first.compareDocumentPosition(second) & Node.DOCUMENT_POSITION_FOLLOWING) !== 0;
    expect(before(port, process)).toBe(true);
    expect(before(process, sentence)).toBe(true);
    expect(before(sentence, label)).toBe(true);
    // The mockup has no Next line, and none of the facts and buttons every Worktree inspector carries.
    expect(panel.queryByText("Next")).not.toBeInTheDocument();
    for (const fact of ["Worktree", "Branch", "Folder", "Slug"]) {
      expect(panel.queryByText(fact)).not.toBeInTheDocument();
    }
    for (const name of ["Open in Editor", "Open in Terminal", "Reveal in Finder"]) {
      expect(panel.queryByRole("button", { name })).not.toBeInTheDocument();
    }
  });

  it("picks a Foreign container too, which has no pid and so no kill command", async () => {
    const calls = app();

    const center = await openWorktree();
    fireEvent.click(within(center).getByText("legacy-db").closest("button") as HTMLElement);

    const panel = within(inspector());
    expect(panel.getByRole("heading", { name: "legacy-db" })).toBeInTheDocument();
    expect(panel.getByText("started outside Procly")).toBeInTheDocument();
    expect(panel.getByText("Foreign process in web-fix")).toBeInTheDocument();
    expect(panel.getByText("5432")).toHaveClass("port");
    expect(panel.getByText("Compose").nextElementSibling).toHaveTextContent("project legacy, service db");
    expect(panel.queryByText(/kill/)).not.toBeInTheDocument();
    expect(panel.queryByRole("button", { name: /^Stop/ })).not.toBeInTheDocument();
    // Nothing is followed: a Foreign process has no output of Procly's.
    expect(calls.map((call) => call.command)).not.toContain("follow_logs");
    expect(calls.map((call) => call.command)).not.toContain("service_logs");
  });

  it("draws no Lease and no Logical resource, only what the protocol returns", async () => {
    app();

    const center = await openWorktree();

    // The word, not the Release button of the screen.
    expect(within(center).queryByText(/\blease\b/i)).not.toBeInTheDocument();
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

  it("gives the directory of a Job the caption that gives way first, so a long one never hides the command", async () => {
    app();
    render(<App store={createDaemonStore()} />);

    fireEvent.click(await screen.findByRole("button", { name: /^Outside Repositories/ }));

    const center = screen.getByRole("main");
    const command = await within(center).findByText("make watch");
    expect(command).toHaveClass("item-name");
    // The stylesheet cuts the directory with an ellipsis before the command loses any width.
    expect(within(center).getByText("/p/tmp/scratch").closest(".item-where")).not.toBeNull();
  });

  it("picks a Job for the inspector, which follows its output from the directory it started in", async () => {
    const calls = app();
    render(<App store={createDaemonStore()} />);
    fireEvent.click(await screen.findByRole("button", { name: /^Outside Repositories/ }));
    const center = screen.getByRole("main");
    await within(center).findByText("make watch");
    expect(within(inspector()).getByText("Select a Job.")).toBeInTheDocument();

    const panel = within(pick(center, "make watch"));

    expect(panel.getByRole("heading", { name: "make watch" })).toBeInTheDocument();
    expect(panel.getByText("c0ffee01")).toBeInTheDocument();
    // A Job has no Stop, and a Job outside any Repository has no Worktree to open.
    expect(panel.queryByRole("button", { name: /stop/i })).not.toBeInTheDocument();
    expect(panel.queryByRole("button", { name: "Open in Editor" })).not.toBeInTheDocument();
    await waitFor(() => {
      expect(calls.filter((call) => call.command === "follow_logs").map((call) => call.args)).toEqual([
        expect.objectContaining({ name: "c0ffee01", cwd: "/p/tmp/scratch" }),
      ]);
    });
  });

  it("says so when no Job started outside a Repository", async () => {
    app({}, { ...(scene as Extract<Response, { response: "status" }>), jobs_outside_repositories: [] });
    render(<App store={createDaemonStore()} />);

    fireEvent.click(await screen.findByRole("button", { name: /^Outside Repositories/ }));

    expect(await screen.findByText("No Job started outside a Repository.")).toBeInTheDocument();
  });

  it("draws a Job as its glyph, then its command, then its state", async () => {
    app();
    render(<App store={createDaemonStore()} />);
    fireEvent.click(await screen.findByRole("button", { name: /^Outside Repositories/ }));
    const center = screen.getByRole("main");
    await within(center).findByText("make watch");

    expect(inOrder(within(center).getByText("make watch").closest("li") as HTMLElement)).toEqual([
      "glyph",
      "command",
      "state",
    ]);
  });
});

/**
 * The scene with its Worktree on a branch that is not the name of its folder, so that a title cannot pass for the
 * branch by chance, and optionally the main Worktree.
 */
function onBranch(branch: string, main = false): Response {
  const status = scene as Extract<Response, { response: "status" }>;
  return {
    ...status,
    repositories: status.repositories.map((repository) => ({
      ...repository,
      worktrees: repository.worktrees.map((worktree) => ({ ...worktree, branch, main })),
    })),
  };
}

/** What a row says, in the order the document draws it: its glyph, its name, then the word of its state. */
function inOrder(row: HTMLElement): string[] {
  const found = (name: string, selector: string): [string, Element] => {
    const part = row.querySelector(selector);
    if (part === null) {
      throw new Error(`the row has no ${name}`);
    }
    return [name, part];
  };
  const parts = [found("glyph", "svg[data-glyph]"), found("command", ".item-name"), found("state", ".word")];
  return parts
    .toSorted(([, first], [, second]) =>
      first.compareDocumentPosition(second) & Node.DOCUMENT_POSITION_FOLLOWING ? -1 : 1,
    )
    .map(([name]) => name);
}

describe("the header of the Worktree screen", () => {
  afterEach(() => {
    cleanup();
    clearMocks();
  });

  /** Opens the Worktree the scene holds, which the sidebar names after its branch. */
  async function open(branch = "feature/login", main = false): Promise<HTMLElement> {
    app({ open_worktree: () => undefined, reveal_worktree: () => undefined }, onBranch(branch, main));
    render(<App store={createDaemonStore()} />);
    fireEvent.click(
      await screen.findByRole("button", { name: new RegExp(`^${branch.replace("/", "\\/")}`) }),
    );
    const center = screen.getByRole("main");
    await within(center).findByRole("heading", { name: "Services" });
    return center;
  }

  /** The commands the window sent to Rust to open or reveal a Worktree. */
  function openings(calls: Call[]): Call[] {
    return calls.filter((call) => call.command.endsWith("_worktree"));
  }

  it("names the Worktree by its folder and shows its path under the name", async () => {
    const center = await open();

    const title = within(center).getByRole("heading", { level: 2, name: "web-fix" });
    expect(within(center).queryByRole("heading", { name: "feature/login" })).not.toBeInTheDocument();
    expect(title.closest("header")?.querySelector(".worktree-path")).toHaveTextContent(PATH);
  });

  it("gives the slug and the other names of the Worktree in the tooltip of the title", async () => {
    const center = await open();

    const tooltip = within(center).getByRole("heading", { level: 2, name: "web-fix" }).closest("[title]");
    expect(tooltip).toHaveAttribute("title", expect.stringContaining("Slug: web_fix"));
    expect(tooltip).toHaveAttribute("title", expect.stringContaining("Branch: feature/login"));
  });

  it("says Main worktree beside the name of the main Worktree, and only of that one", async () => {
    const center = await open("main", true);
    const title = within(center).getByRole("heading", { level: 2, name: "web-fix" });

    expect(title.closest("header")).toHaveTextContent("Main worktree");
    expect(title).not.toHaveTextContent("Main worktree");
    cleanup();
    clearMocks();

    const other = await open();
    expect(
      within(other).getByRole("heading", { level: 2, name: "web-fix" }).closest("header"),
    ).not.toHaveTextContent("Main worktree");
  });

  it("holds Open in Editor and Open in Terminal, which open the Worktree by its path as the inspector did", async () => {
    const calls = app({ open_worktree: () => undefined }, onBranch("feature/login"));
    render(<App store={createDaemonStore()} />);
    fireEvent.click(await screen.findByRole("button", { name: /^feature\/login/ }));
    const header = (await screen.findByRole("heading", { level: 2, name: "web-fix" })).closest("header");
    expect(header).not.toBeNull();

    fireEvent.click(within(header as HTMLElement).getByRole("button", { name: "Open in Editor" }));
    fireEvent.click(within(header as HTMLElement).getByRole("button", { name: "Open in Terminal" }));

    await waitFor(() => {
      expect(openings(calls)).toEqual([
        { command: "open_worktree", args: { path: PATH, with: "editor" } },
        { command: "open_worktree", args: { path: PATH, with: "terminal" } },
      ]);
    });
  });

  it("no longer holds them in the inspector, which keeps Reveal in Finder", async () => {
    const calls = app(
      { open_worktree: () => undefined, reveal_worktree: () => undefined },
      onBranch("feature/login"),
    );
    render(<App store={createDaemonStore()} />);
    fireEvent.click(await screen.findByRole("button", { name: /^feature\/login/ }));
    const center = screen.getByRole("main");
    await within(center).findByRole("heading", { name: "Services" });

    const bare = within(inspector());
    expect(bare.queryByRole("button", { name: /^Open in/ })).not.toBeInTheDocument();
    fireEvent.click(bare.getByRole("button", { name: "Reveal in Finder" }));
    await waitFor(() => {
      expect(openings(calls)).toEqual([{ command: "reveal_worktree", args: { path: PATH } }]);
    });

    const picked = within(pick(center, "web"));
    expect(picked.queryByRole("button", { name: /^Open in/ })).not.toBeInTheDocument();
    expect(picked.getByRole("button", { name: "Reveal in Finder" })).toBeInTheDocument();
  });

  it("shows a failure to open under the header, once", async () => {
    app(
      {
        open_worktree: () => Promise.reject({ kind: "launch", message: "cannot open /p/work/acme/web-fix" }),
      },
      onBranch("feature/login"),
    );
    render(<App store={createDaemonStore()} />);
    fireEvent.click(await screen.findByRole("button", { name: /^feature\/login/ }));
    const header = (await screen.findByRole("heading", { level: 2, name: "web-fix" })).closest("header");

    fireEvent.click(within(header as HTMLElement).getByRole("button", { name: "Open in Terminal" }));

    // Found once: the inspector, which also opens a Worktree from the menu bar, does not repeat it.
    const alert = await screen.findByText("cannot open /p/work/acme/web-fix");
    expect(alert).toHaveAttribute("role", "alert");
    expect(alert.previousElementSibling).toBe(header);
  });
});
