import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { emit } from "@tauri-apps/api/event";
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { afterEach, describe, expect, it } from "vitest";

import { App } from "./App";
import { createDaemonStore, type FailureKey, type Failure, type NeedsYou, type Update } from "./daemon";
import recorded from "./fixtures/status.json";
import overview from "./fixtures/status-overview.json";
import { PortPill } from "./PortPill";
import type { Response } from "./protocol/Response";
import { partsOf } from "./test-rows";

/** A `Status { all: true }` answer recorded from a running Daemon, its paths replaced by neutral ones. */
const status: Response = recorded as Response;

/** An answer with 3 Repositories, 6 Worktrees and every kind of label, in the shape of the recorded one. */
const many: Response = overview as Response;

/** When the poller read the answer. */
const AT = Date.UTC(2026, 10, 5, 19, 35, 0);

function answered(
  response: Response,
  attention: { needsYou?: NeedsYou[]; seen?: FailureKey[] } = {},
  at = AT,
): Update {
  return { at_ms: at, response, needs_you: attention.needsYou ?? [], seen: attention.seen ?? [] };
}

function failed(kind: Exclude<Failure["kind"], "protocol_mismatch">, message: string, at = AT): Update {
  return { at_ms: at, failure: { kind, message }, needs_you: [], seen: [] };
}

/**
 * Plays the Rust side: `last` is what the poller has already answered, and every command the window sends is kept.
 * `onMarkSeen` stands for the Seen set of Rust, which answers a mark with a new update.
 */
function poller(last: Update | null = null, onMarkSeen?: (key: unknown) => Update | void): string[] {
  const commands: string[] = [];
  mockIPC(
    (command, args) => {
      commands.push(command);
      if (command === "last_update") {
        return last;
      }
      if (command === "mark_seen" && onMarkSeen !== undefined) {
        const next = onMarkSeen((args as { key: unknown }).key);
        if (next !== undefined) {
          void emit("daemon-update", next);
        }
        return undefined;
      }
      if (command === "refresh" || command === "stop_follow") {
        return undefined;
      }
      // The inspector follows what is picked: it waits for output that never comes.
      if (command === "follow_logs") {
        return new Promise<void>(() => undefined);
      }
      return Promise.reject(new Error(`unexpected command ${command}`));
    },
    { shouldMockEvents: true },
  );
  return commands;
}

/** What the poller sends the window on its next tick. */
async function tick(update: Update): Promise<void> {
  await act(async () => {
    await emit("daemon-update", update);
  });
}

describe("the window", () => {
  afterEach(() => {
    cleanup();
    clearMocks();
  });

  it("draws the 3 panes and the Worktrees of the answer the poller already has", async () => {
    const commands = poller(answered(status));

    render(<App store={createDaemonStore()} />);

    const sidebar = screen.getByRole("navigation", { name: "Worktrees" });
    expect(
      await within(sidebar).findByRole("button", { name: /^main\s*Main worktree$/ }),
    ).toBeInTheDocument();
    expect(within(sidebar).getByRole("button", { name: "web-fix" })).toBeInTheDocument();
    expect(within(sidebar).getByRole("heading", { name: "web" })).toBeInTheDocument();
    expect(screen.getByRole("main")).toBeInTheDocument();
    expect(screen.getByRole("complementary", { name: "Inspector" })).toBeInTheDocument();
    expect(commands).toContain("last_update");
  });

  it("never asks the Daemon for a Status itself, the poller does, and runs doctor and the Orphan scan once at launch", async () => {
    const commands = poller(answered(status));

    render(<App store={createDaemonStore()} />);
    await screen.findByRole("button", { name: "web-fix" });

    expect(commands).toEqual(["last_update", "doctor", "list_orphans"]);
  });

  it("follows the poller: a new answer replaces the one on screen without a request", async () => {
    poller(null);
    render(<App store={createDaemonStore()} />);
    expect(await screen.findAllByText("Reading the Daemon…")).not.toHaveLength(0);

    await tick(answered(status));
    expect(await screen.findByRole("button", { name: "web-fix" })).toBeInTheDocument();

    await tick(answered(many));
    expect(await screen.findByRole("button", { name: /^docs-old/ })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "web-fix" })).not.toBeInTheDocument();
  });

  it("opens a Worktree from the sidebar", async () => {
    poller(answered(status));
    render(<App store={createDaemonStore()} />);

    fireEvent.click(await screen.findByRole("button", { name: "web-fix" }));

    const center = screen.getByRole("main");
    expect(await within(center).findByText("exited 3")).toBeInTheDocument();
    expect(within(center).getByText("/p/work/acme/web-fix")).toBeInTheDocument();
  });

  it("says that no Daemon runs, and sends nothing but the request for the last answer and doctor", async () => {
    const commands = poller(failed("unreachable", "the daemon did not answer on /p/state/procly.sock"));

    render(<App store={createDaemonStore()} />);

    expect(await screen.findByRole("heading", { name: "The Daemon is not running" })).toBeInTheDocument();
    expect(screen.getByText(/^Procly shows nothing until it runs\./)).toBeInTheDocument();
    // Doctor runs when the app enters that state, and it never starts a Daemon.
    expect(commands).toEqual(["last_update", "doctor"]);
  });

  it("shows why a Daemon that does not answer well failed", async () => {
    poller(failed("decode", "unreadable answer from the daemon"));

    render(<App store={createDaemonStore()} />);

    expect(await screen.findByText("unreadable answer from the daemon")).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "The Daemon is not running" })).not.toBeInTheDocument();
  });

  it("asks the poller for a tick when Refresh is pressed, runs doctor and the Orphan scan, and does not ask the Daemon for a Status", async () => {
    const commands = poller(answered(status));
    render(<App store={createDaemonStore()} />);

    await screen.findByRole("region", { name: "Overview" });
    fireEvent.click(screen.getByRole("button", { name: "Refresh" }));

    await act(async () => {
      await Promise.resolve();
    });
    expect(commands).toEqual(["last_update", "doctor", "list_orphans", "refresh", "doctor", "list_orphans"]);
  });
});

describe("the Overview", () => {
  afterEach(() => {
    cleanup();
    clearMocks();
  });

  it("is what the window opens on, and lists every Service of every Worktree once, flat, with its port as a pill", async () => {
    poller(answered(many));

    render(<App store={createDaemonStore()} />);

    const overviewPane = await screen.findByRole("region", { name: "Overview" });
    const rows = within(overviewPane).getAllByRole("listitem");
    expect(rows.map(partsOf)).toEqual([
      { glyph: "healthy", name: "api", word: "healthy", where: "web", ports: ["18731"] },
      { glyph: "busy", name: "worker", word: "restarting", where: "web", ports: [] },
      { glyph: "ended", name: "api", word: "exited 1", where: "web-login", ports: ["18742"] },
      { glyph: "busy", name: "site", word: "starting", where: "docs-preview", ports: ["18755"] },
      { glyph: "ended", name: "site", word: "stopped", where: "docs-oldRemoved", ports: ["18760"] },
    ]);
    // A port is a pill, never the text ":18731", and it is the pill the shared component draws.
    expect(within(overviewPane).queryByText(/^:\d+$/)).not.toBeInTheDocument();
    const pill = overviewPane.querySelector(".port");
    const { container } = render(<PortPill port={18731} />);
    expect(pill?.outerHTML).toBe(container.innerHTML);
    // The Services are one list across the Worktrees, not a list under the heading of each Worktree.
    expect(within(overviewPane).getAllByRole("list")).toHaveLength(1);
  });

  it("starts every row with the state glyph, the word of the state following the name", async () => {
    poller(answered(many));

    render(<App store={createDaemonStore()} />);

    const overviewPane = await screen.findByRole("region", { name: "Overview" });
    for (const row of within(overviewPane).getAllByRole("listitem")) {
      const select = row.querySelector(".item-select");
      expect(select?.firstElementChild?.querySelector("svg")).toHaveAttribute("data-glyph");
      const main = select?.querySelector(".item-main");
      expect(main?.firstElementChild).toHaveClass("item-name");
      expect(main?.lastElementChild).toHaveClass("word");
    }
  });

  it("lists the Isolated Compose services of each Worktree with their port, a Worktree with nothing else included", async () => {
    const withContainers = structuredClone(many);
    if (withContainers.response !== "status") {
      throw new Error("the recorded answer is a status");
    }
    const worktrees = withContainers.repositories.flatMap((repository) => repository.worktrees);
    const db = { service: "db", project: "acme-web", compose: "postgres", port: 24050 };
    const cache = { service: "cache", project: "acme-infra", compose: "redis", port: null };
    const web = worktrees.find((worktree) => worktree.path === "/p/work/acme/web");
    const infra = worktrees.find((worktree) => worktree.path === "/p/work/acme/infra");
    if (web === undefined || infra === undefined) {
      throw new Error("the recorded answer has the web and infra Worktrees");
    }
    web.containers = [db];
    // The infra Worktree runs no Process service, only this container.
    infra.containers = [cache];
    poller(answered(withContainers));

    render(<App store={createDaemonStore()} />);

    const overviewPane = await screen.findByRole("region", { name: "Overview" });
    const rows = within(overviewPane).getAllByRole("listitem").map(partsOf);
    expect(rows).toEqual([
      { glyph: "healthy", name: "api", word: "healthy", where: "web", ports: ["18731"] },
      { glyph: "busy", name: "worker", word: "restarting", where: "web", ports: [] },
      { glyph: "healthy", name: "db", word: "running", where: "web", ports: ["24050"] },
      { glyph: "ended", name: "api", word: "exited 1", where: "web-login", ports: ["18742"] },
      { glyph: "busy", name: "site", word: "starting", where: "docs-preview", ports: ["18755"] },
      { glyph: "ended", name: "site", word: "stopped", where: "docs-oldRemoved", ports: ["18760"] },
      { glyph: "healthy", name: "cache", word: "running", where: "infra", ports: [] },
    ]);
  });

  it("names the Worktree of a row by its folder, and gives its branch, folder and slug in a tooltip", async () => {
    poller(answered(many));

    render(<App store={createDaemonStore()} />);

    const overviewPane = await screen.findByRole("region", { name: "Overview" });
    const where = overviewPane.querySelectorAll(".item-where");
    expect(where[2]).toHaveTextContent("web-login");
    expect(where[2]).toHaveAttribute(
      "title",
      "Branch: feature/a-very-long-branch-name-that-the-sidebar-has-to-cut-at-its-end\nFolder: web-login\nSlug: web_login",
    );
  });

  it("shows when each Service started as an absolute time, never as an age, and when the answer was read", async () => {
    poller(answered(many));

    render(<App store={createDaemonStore()} />);

    const overviewPane = await screen.findByRole("region", { name: "Overview" });
    expect(within(overviewPane).queryByText(/\bago\b/)).not.toBeInTheDocument();
    expect(within(overviewPane).getByText("2026-11-04 19:31:22")).toBeInTheDocument();
    expect(screen.getByText("Updated at 19:35:00")).toBeInTheDocument();
  });

  it("puts the title and the time of the last answer on the same line, with Refresh", async () => {
    poller(answered(many));

    render(<App store={createDaemonStore()} />);

    const title = await screen.findByRole("heading", { name: "Overview", level: 2 });
    const line = title.parentElement;
    expect(line).not.toBeNull();
    expect(line).toContainElement(screen.getByText("Updated at 19:35:00"));
    expect(line).toContainElement(screen.getByRole("button", { name: "Refresh" }));
    // Nothing sits above the line.
    expect(screen.getByRole("main").querySelector("header.toolbar")).toBeNull();
  });

  it("says that nothing runs yet when no Service is known, with the command that steers the agents to Procly", async () => {
    poller(answered(status));
    render(<App store={createDaemonStore()} />);
    await screen.findByRole("region", { name: "Overview" });

    await tick(answered({ response: "status", repositories: [], jobs_outside_repositories: [] }));

    const overviewPane = await screen.findByRole("region", { name: "Overview" });
    expect(within(overviewPane).getByRole("img", { name: "Procly" })).toBeInTheDocument();
    expect(within(overviewPane).getByRole("heading", { name: "Nothing is running yet" })).toBeInTheDocument();
    expect(
      within(overviewPane).getByText(
        "Your coding agents start Services and Jobs through Procly, and they appear here. To steer them to Procly, install the Guard and the Rules block in a Repository.",
      ),
    ).toBeInTheDocument();
    const box = overviewPane.querySelector(".copybox");
    expect(box).toHaveTextContent("procly agent install");
    expect(within(box as HTMLElement).getByRole("button", { name: "Copy" })).toBeInTheDocument();
    expect(screen.queryByText("No Service is known.")).not.toBeInTheDocument();
  });

  it("keeps the title line and Refresh when nothing runs, so that the window can still be asked for a tick", async () => {
    poller(answered({ response: "status", repositories: [], jobs_outside_repositories: [] }));

    render(<App store={createDaemonStore()} />);

    const title = await screen.findByRole("heading", { name: "Overview", level: 2 });
    expect(title.parentElement).toContainElement(screen.getByRole("button", { name: "Refresh" }));
  });

  it("is not the empty Overview when a Foreign process needs the developer, whatever runs", async () => {
    poller(
      answered(
        { response: "status", repositories: [], jobs_outside_repositories: [] },
        { needsYou: [FOREIGN] },
      ),
    );

    render(<App store={createDaemonStore()} />);

    expect(await screen.findByRole("region", { name: "Needs you" })).toBeInTheDocument();
    expect(screen.queryByText("Nothing is running yet")).not.toBeInTheDocument();
  });

  it("comes back with ⌘0 from a Worktree, which the Overview item of the View menu carries", async () => {
    poller(answered(many));
    render(<App store={createDaemonStore()} />);
    fireEvent.click(await screen.findByRole("button", { name: "docs-preview" }));
    expect(screen.queryByRole("region", { name: "Overview" })).not.toBeInTheDocument();

    await act(async () => {
      await emit("menu-choice", "overview");
    });

    expect(await screen.findByRole("region", { name: "Overview" })).toBeInTheDocument();
  });
});

describe("the inspector of a Worktree", () => {
  afterEach(() => {
    cleanup();
    clearMocks();
  });

  const BRANCH = "feature/a-very-long-branch-name-that-the-sidebar-has-to-cut-at-its-end";

  /** The facts the inspector lists, as `name: value` pairs in the order it lists them. */
  function facts(): string[] {
    const inspector = screen.getByRole("complementary", { name: "Inspector" });
    return within(inspector)
      .getAllByRole("term")
      .map((term) => `${term.textContent}: ${term.nextElementSibling?.textContent ?? ""}`);
  }

  it("names the branch, the folder and the slug of the Worktree on screen", async () => {
    poller(answered(many));
    render(<App store={createDaemonStore()} />);

    fireEvent.click(await screen.findByRole("button", { name: BRANCH }));
    // The title names the folder, the branch is one of the names the inspector gives.
    await screen.findByRole("heading", { name: "web-login", level: 2 });

    expect(facts()).toEqual(
      expect.arrayContaining([`Branch: ${BRANCH}`, "Folder: web-login", "Slug: web_login"]),
    );
  });

  it("names the branch, the folder and the slug of the Worktree of the item picked", async () => {
    poller(answered(many));
    render(<App store={createDaemonStore()} />);
    fireEvent.click(await screen.findByRole("button", { name: BRANCH }));
    const center = screen.getByRole("main");

    fireEvent.click(await within(center).findByRole("button", { name: /^api/ }));

    expect(facts()).toEqual(
      expect.arrayContaining([`Branch: ${BRANCH}`, "Folder: web-login", "Slug: web_login"]),
    );
  });

  it("says a Worktree with no branch has none", async () => {
    poller(answered(many));
    render(<App store={createDaemonStore()} />);

    fireEvent.click(await screen.findByRole("button", { name: "docs-preview" }));
    await screen.findByRole("heading", { name: "docs-preview", level: 2 });

    expect(facts()).toEqual(expect.arrayContaining(["Branch: none", "Folder: docs-preview"]));
  });
});

describe("the sidebar", () => {
  afterEach(() => {
    cleanup();
    clearMocks();
  });

  it("draws no title above the entries, the traffic lights sit on its background", async () => {
    poller(answered(many));

    render(<App store={createDaemonStore()} />);

    const sidebar = screen.getByRole("navigation", { name: "Worktrees" });
    await within(sidebar).findByRole("button", { name: "docs-preview" });
    expect(within(sidebar).queryByRole("heading", { level: 1 })).toBeNull();
    expect(screen.queryByRole("heading", { name: "Procly" })).toBeNull();
  });

  it("opens with a drag region that holds no entry, so that the entries stay clickable", async () => {
    poller(answered(many));

    render(<App store={createDaemonStore()} />);

    const sidebar = screen.getByRole("navigation", { name: "Worktrees" });
    await within(sidebar).findByRole("button", { name: "docs-preview" });
    const region = sidebar.querySelector("[data-tauri-drag-region]");
    expect(region).not.toBeNull();
    expect(sidebar.firstElementChild).toBe(region);
    expect(region?.children).toHaveLength(0);
    expect(region?.textContent).toBe("");
  });

  it("lists the Worktrees by branch, by folder name when there is no branch, and marks the removed ones", async () => {
    poller(answered(many));

    render(<App store={createDaemonStore()} />);

    const sidebar = screen.getByRole("navigation", { name: "Worktrees" });
    await within(sidebar).findByRole("button", { name: "docs-preview" });
    const labels = within(sidebar)
      .getAllByRole("button")
      .map((button) => button.textContent)
      .filter(
        (text) =>
          text !== "Overview" &&
          !text?.startsWith("Outside Repositories") &&
          text !== "Orphans" &&
          text !== "Doctor" &&
          text !== "Settings⌘,",
      );
    expect(labels).toEqual([
      "mainMain worktree",
      "feature/a-very-long-branch-name-that-the-sidebar-has-to-cut-at-its-end",
      "mainMain worktree",
      "docs-preview",
      "docs-oldRemoved",
      "mainMain worktree",
    ]);
    expect(within(sidebar).queryByText(/detached/i)).not.toBeInTheDocument();
    expect(within(sidebar).getAllByText("Removed")).toHaveLength(1);
  });

  it("puts the Overview first, open", async () => {
    poller(answered(many));

    render(<App store={createDaemonStore()} />);

    const sidebar = screen.getByRole("navigation", { name: "Worktrees" });
    const overviewButton = await within(sidebar).findByRole("button", { name: /^Overview/ });
    expect(overviewButton).toHaveAttribute("aria-current", "true");
    expect(within(sidebar).getAllByRole("button")[0]).toBe(overviewButton);
  });

  it("names the branch, the folder and the slug in the tooltip, and cuts a long name at its end", async () => {
    poller(answered(many));

    render(<App store={createDaemonStore()} />);

    const sidebar = screen.getByRole("navigation", { name: "Worktrees" });
    const long = await within(sidebar).findByRole("button", {
      name: "feature/a-very-long-branch-name-that-the-sidebar-has-to-cut-at-its-end",
    });
    expect(long).toHaveAttribute(
      "title",
      "Branch: feature/a-very-long-branch-name-that-the-sidebar-has-to-cut-at-its-end\nFolder: web-login\nSlug: web_login",
    );
    expect(within(long).getByText(/^feature\//)).toHaveClass("row-name");
    const detached = within(sidebar).getByRole("button", { name: "docs-preview" });
    expect(detached).toHaveAttribute("title", "Branch: none\nFolder: docs-preview\nSlug: docs_preview");
  });

  it("captions the Main worktree beside its label, and the label alone is the name of the row", async () => {
    poller(answered(status));

    render(<App store={createDaemonStore()} />);

    const sidebar = screen.getByRole("navigation", { name: "Worktrees" });
    const main = await within(sidebar).findByRole("button", { name: /^main\s*Main worktree$/ });
    expect(within(main).getByText("Main worktree")).toBeInTheDocument();
    expect(main.querySelector(".row-name")).toHaveTextContent(/^main$/);
    // The legend sits beside the label, outside the element that carries it, and "main" is not said twice.
    expect(main.querySelector(".row-name")).not.toContainElement(within(main).getByText("Main worktree"));
    expect(within(main).getAllByText(/main/i)).toHaveLength(2);
    expect(within(main).queryByText("main", { selector: ".caption" })).toBeNull();
  });

  it("gives a Worktree that is not the Main worktree no legend", async () => {
    poller(answered(status));

    render(<App store={createDaemonStore()} />);

    const sidebar = screen.getByRole("navigation", { name: "Worktrees" });
    const fix = await within(sidebar).findByRole("button", { name: "web-fix" });
    expect(within(fix).queryByText("Main worktree")).toBeNull();
  });

  describe("the Outside Repositories entry", () => {
    const OUTSIDE_JOB = { id: "c0ffee01", command: "make watch", cwd: "/p/tmp/scratch", state: "running" };

    function outsideEntry(jobs: object[]) {
      const base = status as Extract<Response, { response: "status" }>;
      poller(answered({ ...base, jobs_outside_repositories: jobs } as Response));
      render(<App store={createDaemonStore()} />);
      const sidebar = screen.getByRole("navigation", { name: "Worktrees" });
      return within(sidebar).findByRole("button", { name: /^Outside Repositories/ });
    }

    it("says how many Jobs started outside a Repository, in the singular for 1", async () => {
      const entry = await outsideEntry([OUTSIDE_JOB]);

      expect(entry).toHaveTextContent(/^Outside Repositories\s*1 Job$/);
      expect(within(entry).getByText("1 Job")).toBeInTheDocument();
    });

    it("says it in the plural for 2", async () => {
      const entry = await outsideEntry([OUTSIDE_JOB, { ...OUTSIDE_JOB, id: "c0ffee02" }]);

      expect(entry).toHaveTextContent(/^Outside Repositories\s*2 Jobs$/);
      expect(within(entry).getByText("2 Jobs")).toBeInTheDocument();
    });

    it("keeps the entry and draws no count when no Job started outside a Repository", async () => {
      const entry = await outsideEntry([]);

      expect(entry).toHaveAccessibleName("Outside Repositories");
      expect(within(entry).queryByText(/Job/)).toBeNull();
    });
  });
});

/** `api` of `web-login` ended with code 1: the one failure of the recorded answer. */
const API_FAILURE: NeedsYou = {
  kind: "service",
  worktree: "/p/work/acme/web-login",
  service: "api",
  started_at: 1793820682000,
  end: { kind: "exited", code: 1 },
};

const API_KEY: FailureKey = {
  kind: "service",
  worktree: "/p/work/acme/web-login",
  service: "api",
  started_at: 1793820682000,
};

const FOREIGN: NeedsYou = {
  kind: "foreign",
  worktree: "/p/work/acme/web",
  process: { kind: "process", pid: 4242, command: "node server.js", ports: [3000] },
};

describe("Needs you", () => {
  afterEach(() => {
    cleanup();
    clearMocks();
  });

  /** Picks the row of Needs you whose name starts with `name`, then marks it as Seen from the inspector. */
  async function markAsSeen(name: RegExp): Promise<void> {
    const needs = await screen.findByRole("region", { name: "Needs you" });
    fireEvent.click(within(needs).getByRole("button", { name }));
    const panel = await screen.findByRole("region", { name: "Selection" });
    fireEvent.click(within(panel).getByRole("button", { name: "Mark as Seen" }));
  }

  /** The rows of the Overview that are not in "Needs you". */
  function serviceRows(): HTMLElement[] {
    const overviewPane = screen.getByRole("region", { name: "Overview" });
    const needs = within(overviewPane).queryByRole("region", { name: "Needs you" });
    return within(overviewPane)
      .getAllByRole("listitem")
      .filter((row) => needs === null || !needs.contains(row));
  }

  /** The row of the ended `api` of `web-login`, wherever the Overview draws it. */
  function apiRow(): HTMLElement | undefined {
    const overviewPane = screen.getByRole("region", { name: "Overview" });
    return within(overviewPane)
      .getAllByRole("listitem")
      .find((row) => partsOf(row).name === "api" && partsOf(row).where === "web-login");
  }

  it("is pinned on top of the Overview, with the failures and the Foreign processes it was given", async () => {
    poller(answered(many, { needsYou: [API_FAILURE, FOREIGN] }));

    render(<App store={createDaemonStore()} />);

    const overviewPane = await screen.findByRole("region", { name: "Overview" });
    const needs = within(overviewPane).getByRole("region", { name: "Needs you" });
    expect(within(needs).getByRole("heading", { name: /^Needs you\s*2$/ })).toBeInTheDocument();
    // The same glyph as everywhere else, first in the row: a cross for the failure, a diamond for the Foreign process.
    expect(within(needs).getAllByRole("listitem").map(partsOf)).toEqual([
      { glyph: "failure", name: "api", word: "exited 1", where: "web-login", ports: ["18742"] },
      {
        glyph: "foreign",
        name: "node server.js",
        word: "started outside Procly",
        where: "web",
        ports: ["3000"],
      },
    ]);
    const firstService = within(overviewPane).getAllByText("healthy")[0];
    expect(
      needs.compareDocumentPosition(firstService as Node) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
    // No row carries the button, it lives in the inspector of the selection.
    expect(screen.queryByRole("button", { name: "Mark as Seen" })).not.toBeInTheDocument();
  });

  it("lists a failed Service once: in Needs you, and not again among the Services", async () => {
    poller(answered(many, { needsYou: [API_FAILURE] }));

    render(<App store={createDaemonStore()} />);

    const overviewPane = await screen.findByRole("region", { name: "Overview" });
    const needs = within(overviewPane).getByRole("region", { name: "Needs you" });
    const failed = (row: HTMLElement): boolean =>
      partsOf(row).name === "api" && partsOf(row).where === "web-login";
    expect(within(needs).getAllByRole("listitem").filter(failed)).toHaveLength(1);
    expect(serviceRows().filter(failed)).toHaveLength(0);
    expect(within(overviewPane).getAllByRole("listitem").filter(failed)).toHaveLength(1);
  });

  it("lists every Service of every Worktree among the Services, without those that Needs you holds", async () => {
    poller(answered(many, { needsYou: [API_FAILURE, FOREIGN] }));
    render(<App store={createDaemonStore()} />);
    await screen.findByRole("region", { name: "Needs you" });

    expect(serviceRows().map(partsOf)).toEqual([
      { glyph: "healthy", name: "api", word: "healthy", where: "web", ports: ["18731"] },
      { glyph: "busy", name: "worker", word: "restarting", where: "web", ports: [] },
      { glyph: "busy", name: "site", word: "starting", where: "docs-preview", ports: ["18755"] },
      { glyph: "ended", name: "site", word: "stopped", where: "docs-oldRemoved", ports: ["18760"] },
    ]);
  });

  it("opens each section with a heading, the Services saying which ones it lists", async () => {
    poller(answered(many, { needsYou: [API_FAILURE] }));

    render(<App store={createDaemonStore()} />);

    const overviewPane = await screen.findByRole("region", { name: "Overview" });
    expect(
      within(overviewPane)
        .getAllByRole("heading", { level: 3 })
        .map((heading) => heading.textContent),
    ).toEqual(["Needs you 1", "Services every Worktree, without those above"]);
  });

  it("puts the glyph first in a row of Needs you for a Service, a Job and a Foreign process", async () => {
    const job: NeedsYou = {
      kind: "job",
      worktree: "/p/work/acme/web",
      id: "ea79fa81",
      command: "bun test",
      end: { kind: "exited", code: 2 },
    };
    poller(answered(many, { needsYou: [API_FAILURE, job, FOREIGN] }));

    render(<App store={createDaemonStore()} />);

    const needs = await screen.findByRole("region", { name: "Needs you" });
    const rows = within(needs).getAllByRole("listitem");
    expect(rows.map(partsOf)).toEqual([
      { glyph: "failure", name: "api", word: "exited 1", where: "web-login", ports: ["18742"] },
      { glyph: "failure", name: "bun test", word: "exited 2", where: "web", ports: [] },
      {
        glyph: "foreign",
        name: "node server.js",
        word: "started outside Procly",
        where: "web",
        ports: ["3000"],
      },
    ]);
    for (const row of rows) {
      const first = row.querySelector(".item-select")?.firstElementChild;
      expect(first?.querySelector("svg")).not.toBeNull();
      expect(first?.textContent).toBe("");
    }
  });

  it("selects a failed Service of Needs you into the inspector, which offers Mark as Seen", async () => {
    poller(answered(many, { needsYou: [API_FAILURE, FOREIGN] }));
    render(<App store={createDaemonStore()} />);
    const needs = await screen.findByRole("region", { name: "Needs you" });
    const inspector = screen.getByRole("complementary", { name: "Inspector" });
    expect(within(inspector).getByText("Select a Service or a Job to see its output.")).toBeInTheDocument();

    fireEvent.click(within(needs).getByRole("button", { name: /^api/ }));

    const panel = await within(inspector).findByRole("region", { name: "Selection" });
    expect(within(panel).getByRole("heading", { name: "api" })).toBeInTheDocument();
    expect(within(panel).getByRole("button", { name: "Mark as Seen" })).toBeInTheDocument();
    expect(within(needs).getByRole("button", { name: /^api/ })).toHaveAttribute("aria-pressed", "true");
    expect(within(needs).getByRole("button", { name: /^api/ }).closest("li")).toHaveAttribute(
      "data-selected",
      "true",
    );
  });

  it("puts Mark as Seen beside the state phrase of the inspector", async () => {
    poller(answered(many, { needsYou: [API_FAILURE] }));
    render(<App store={createDaemonStore()} />);
    const needs = await screen.findByRole("region", { name: "Needs you" });

    fireEvent.click(within(needs).getByRole("button", { name: /^api/ }));

    const panel = await screen.findByRole("region", { name: "Selection" });
    const button = within(panel).getByRole("button", { name: "Mark as Seen" });
    expect(button.parentElement).toContainElement(panel.querySelector(".state"));
  });

  it("offers no Mark as Seen for a Service that ended in a routine way, or a failure already Seen", async () => {
    poller(answered(many, { seen: [API_KEY] }));
    render(<App store={createDaemonStore()} />);
    await screen.findByRole("region", { name: "Overview" });

    fireEvent.click(within(apiRow() as HTMLElement).getByRole("button"));

    const panel = await screen.findByRole("region", { name: "Selection" });
    expect(within(panel).getByRole("heading", { name: "api" })).toBeInTheDocument();
    expect(within(panel).queryByRole("button", { name: "Mark as Seen" })).not.toBeInTheDocument();

    // Rust lists no failure any more: the same Service ended in a routine way.
    await tick(answered(many));
    expect(within(panel).queryByRole("button", { name: "Mark as Seen" })).not.toBeInTheDocument();
  });

  it("picks a Foreign process of Needs you for the inspector, with the kill command and no Mark as Seen", async () => {
    const withForeign: Response = JSON.parse(JSON.stringify(many)) as Response;
    if (withForeign.response !== "status") {
      throw new Error("the recorded answer is a status");
    }
    const web = withForeign.repositories
      .flatMap((repository) => repository.worktrees)
      .find((worktree) => worktree.path === FOREIGN.worktree);
    if (web === undefined || FOREIGN.kind !== "foreign") {
      throw new Error("the recorded answer holds the Worktree of the Foreign process");
    }
    web.foreign = [FOREIGN.process];
    poller(answered(withForeign, { needsYou: [FOREIGN] }));
    render(<App store={createDaemonStore()} />);
    const needs = await screen.findByRole("region", { name: "Needs you" });

    const row = within(needs).getByRole("button");
    expect(row).toHaveAttribute("aria-pressed", "false");
    fireEvent.click(row);

    expect(row).toHaveAttribute("aria-pressed", "true");
    const panel = await screen.findByRole("region", { name: "Selection" });
    expect(within(panel).getByRole("heading", { name: "node server.js" })).toBeInTheDocument();
    expect(within(panel).getByText("kill 4242")).toBeInTheDocument();
    expect(within(panel).queryByRole("button", { name: "Mark as Seen" })).not.toBeInTheDocument();
    expect(within(panel).queryByRole("button", { name: /^Stop/ })).not.toBeInTheDocument();
  });

  it("draws an ended row as a failure when Rust lists it, unseen or Seen, and as routine otherwise", async () => {
    /** The tone of the state of the ended `api` row of the Overview. */
    const toneOfApi = (): string | undefined =>
      apiRow()
        ?.querySelector(".word")
        ?.className.split(" ")
        .find((name) => name.startsWith("tone-"));

    poller(answered(many, { needsYou: [API_FAILURE] }));
    render(<App store={createDaemonStore()} />);
    await screen.findByRole("region", { name: "Overview" });
    expect(toneOfApi()).toBe("tone-fail");

    await tick(answered(many, { seen: [API_KEY] }));
    expect(toneOfApi()).toBe("tone-fail");

    // Rust lists no failure: the window keeps no list of its own that would call `exited 1` one.
    await tick(answered(many));
    expect(toneOfApi()).toBe("tone-ended");
  });

  it("is left out when nothing needs the developer, however many Services ended", async () => {
    poller(answered(many));

    render(<App store={createDaemonStore()} />);

    const overviewPane = await screen.findByRole("region", { name: "Overview" });
    expect(within(overviewPane).queryByRole("region", { name: "Needs you" })).not.toBeInTheDocument();
    expect(within(overviewPane).getByText("exited 1")).toBeInTheDocument();
  });

  it("marks a failure as Seen: it leaves the list and the count while its row stays, dimmed", async () => {
    const marked: unknown[] = [];
    poller(answered(many, { needsYou: [API_FAILURE, FOREIGN] }), (key) => {
      marked.push(key);
      return answered(many, { needsYou: [FOREIGN], seen: [API_KEY] });
    });
    render(<App store={createDaemonStore()} />);
    await markAsSeen(/^api/);

    await waitFor(() => {
      expect(within(screen.getByRole("region", { name: "Needs you" })).getAllByRole("listitem")).toHaveLength(
        1,
      );
    });
    expect(marked).toEqual([API_KEY]);
    const left = screen.getByRole("region", { name: "Needs you" });
    expect(within(left).getByRole("heading", { name: /^Needs you\s*1$/ })).toBeInTheDocument();
    expect(within(left).queryByText("exited 1")).not.toBeInTheDocument();
    const row = apiRow();
    expect(row).toBeDefined();
    expect(serviceRows()).toContain(row);
    expect(row).toHaveAttribute("data-seen", "true");
    expect(row).toHaveTextContent("Seen");
    // The healthy Service of the same answer is not dimmed.
    expect(serviceRows().filter((candidate) => candidate.hasAttribute("data-seen"))).toHaveLength(1);
  });

  it("leaves the section out once the last line is Seen", async () => {
    poller(answered(many, { needsYou: [API_FAILURE] }), () =>
      answered(many, { needsYou: [], seen: [API_KEY] }),
    );
    render(<App store={createDaemonStore()} />);

    await markAsSeen(/^api/);

    await waitFor(() => {
      expect(screen.queryByRole("region", { name: "Needs you" })).not.toBeInTheDocument();
    });
    expect(serviceRows().some((row) => row.hasAttribute("data-seen"))).toBe(true);
  });

  it("brings a failure back when its Service started again and failed again", async () => {
    poller(answered(many, { seen: [API_KEY] }));
    render(<App store={createDaemonStore()} />);
    await screen.findByRole("region", { name: "Overview" });
    expect(screen.queryByRole("region", { name: "Needs you" })).not.toBeInTheDocument();

    await tick(
      answered(many, {
        needsYou: [{ ...API_FAILURE, started_at: API_FAILURE.started_at + 60_000 }],
        seen: [],
      }),
    );

    expect(await screen.findByRole("region", { name: "Needs you" })).toBeInTheDocument();
  });

  describe("counter on the Overview entry", () => {
    const SECOND_FOREIGN: NeedsYou = {
      kind: "foreign",
      worktree: "/p/work/acme/web",
      process: { kind: "process", pid: 4343, command: "node worker.js", ports: [3001] },
    };

    /** The Overview entry of the sidebar. */
    async function overviewEntry(): Promise<HTMLElement> {
      const sidebar = screen.getByRole("navigation", { name: "Worktrees" });
      return within(sidebar).findByRole("button", { name: /^Overview/ });
    }

    it("says how many items need the developer, as many as the Needs you section lists", async () => {
      poller(answered(many, { needsYou: [API_FAILURE, FOREIGN, SECOND_FOREIGN] }));

      render(<App store={createDaemonStore()} />);

      const entry = await overviewEntry();
      expect(within(entry).getByText("3 need you")).toBeInTheDocument();
      const needs = await screen.findByRole("region", { name: "Needs you" });
      expect(within(needs).getAllByRole("listitem")).toHaveLength(3);
    });

    it("is drawn as the count of a sidebar entry, not as a caption", async () => {
      poller(answered(many, { needsYou: [API_FAILURE, FOREIGN, SECOND_FOREIGN] }));

      render(<App store={createDaemonStore()} />);

      const count = within(await overviewEntry()).getByText("3 need you");
      expect(count).toHaveClass("row-count");
      expect(count).not.toHaveClass("caption");
    });

    it("says 'needs' for a single item", async () => {
      poller(answered(many, { needsYou: [FOREIGN] }));

      render(<App store={createDaemonStore()} />);

      expect(within(await overviewEntry()).getByText("1 needs you")).toBeInTheDocument();
    });

    it("shows no count, and no shortcut either, when nothing needs the developer", async () => {
      poller(answered(many));

      render(<App store={createDaemonStore()} />);

      const entry = await overviewEntry();
      expect(entry).toHaveTextContent(/^Overview$/);
    });

    it("drops by 1 when a failure is marked as Seen", async () => {
      poller(answered(many, { needsYou: [API_FAILURE, FOREIGN, SECOND_FOREIGN] }), () =>
        answered(many, { needsYou: [FOREIGN, SECOND_FOREIGN], seen: [API_KEY] }),
      );
      render(<App store={createDaemonStore()} />);
      expect(within(await overviewEntry()).getByText("3 need you")).toBeInTheDocument();

      await markAsSeen(/^api/);

      await waitFor(() => {
        expect(
          within(screen.getByRole("navigation", { name: "Worktrees" })).getByText("2 need you"),
        ).toBeInTheDocument();
      });
    });
  });

  it("marks a Job as Seen by its id and dims its row in its Worktree", async () => {
    const job = { id: "j-7", command: "make test", cwd: "/p/work/acme/web" };
    const withJob: Response = {
      ...status,
      repositories:
        status.response === "status"
          ? status.repositories.map((repository) => ({
              ...repository,
              worktrees: repository.worktrees.map((worktree) =>
                worktree.path === "/p/work/acme/web"
                  ? {
                      ...worktree,
                      jobs: [{ ...job, state: "ended" as const, end: { kind: "exited" as const, code: 2 } }],
                    }
                  : worktree,
              ),
            }))
          : [],
    } as Response;
    const failure: NeedsYou = {
      kind: "job",
      worktree: "/p/work/acme/web",
      id: "j-7",
      command: "make test",
      end: { kind: "exited", code: 2 },
    };
    const marked: unknown[] = [];
    poller(answered(withJob, { needsYou: [failure] }), (key) => {
      marked.push(key);
      return answered(withJob, { seen: [{ kind: "job", id: "j-7" }] });
    });
    render(<App store={createDaemonStore()} />);

    // The inspector of a failed Job offers it as well.
    await markAsSeen(/^make test/);
    await waitFor(() => {
      expect(screen.queryByRole("region", { name: "Needs you" })).not.toBeInTheDocument();
    });
    fireEvent.click(screen.getByRole("button", { name: /^main\s*Main worktree$/ }));

    expect(marked).toEqual([{ kind: "job", id: "j-7" }]);
    const row = (await screen.findByText("make test")).closest("li");
    expect(row).toHaveAttribute("data-seen", "true");
  });
});

describe("the reason line of a Worktree in the sidebar", () => {
  afterEach(() => {
    cleanup();
    clearMocks();
  });

  const LONG_BRANCH = "feature/a-very-long-branch-name-that-the-sidebar-has-to-cut-at-its-end";

  /** The sidebar entry of the Worktree whose folder is `folder`, found by the tooltip that names it. */
  async function entryOf(folder: string): Promise<HTMLElement> {
    const sidebar = screen.getByRole("navigation", { name: "Worktrees" });
    await within(sidebar).findByRole("button", { name: /^Overview/ });
    const entry = within(sidebar)
      .getAllByRole("button")
      .find((button) => button.getAttribute("title")?.includes(`Folder: ${folder}\n`));
    if (entry === undefined) {
      throw new Error(`no sidebar entry for ${folder}`);
    }
    return entry;
  }

  /** The reason lines of the whole sidebar. */
  function reasons(): Element[] {
    return [...screen.getByRole("navigation", { name: "Worktrees" }).querySelectorAll(".row-reason")];
  }

  it("says under the label why a Worktree needs the developer, with the glyph of the failed Service", async () => {
    poller(answered(many, { needsYou: [API_FAILURE] }));

    render(<App store={createDaemonStore()} />);

    const entry = await entryOf("web-login");
    const reason = entry.querySelector(".row-reason");
    expect(reason).toHaveTextContent("api exited 1");
    expect(reason?.querySelector("svg")).toHaveAttribute("data-glyph", "failure");
    // The label stays on its own line, above the reason.
    expect(entry.querySelector(".row-line")).toHaveTextContent(LONG_BRANCH);
    expect(entry.querySelector(".row-line")).not.toContainElement(reason as HTMLElement);
    // The glyph carries the state, the colour class repeats it.
    expect(reason).toHaveClass("tone-fail");
  });

  it("names a Foreign process as a process outside Procly, with the glyph of a Foreign process", async () => {
    poller(answered(many, { needsYou: [FOREIGN] }));

    render(<App store={createDaemonStore()} />);

    const reason = (await entryOf("web")).querySelector(".row-reason");
    expect(reason).toHaveTextContent("a process outside Procly");
    expect(reason?.querySelector("svg")).toHaveAttribute("data-glyph", "foreign");
    expect(reason).toHaveClass("tone-warn");
  });

  it("names a failed Job by its command", async () => {
    const job: NeedsYou = {
      kind: "job",
      worktree: "/p/work/acme/web",
      id: "j-7",
      command: "make test",
      end: { kind: "exited", code: 2 },
    };
    poller(answered(many, { needsYou: [job] }));

    render(<App store={createDaemonStore()} />);

    const reason = (await entryOf("web")).querySelector(".row-reason");
    expect(reason).toHaveTextContent("make test exited 2");
    expect(reason?.querySelector("svg")).toHaveAttribute("data-glyph", "failure");
  });

  it("puts a failure before a Foreign process of the same Worktree, whatever their order in Needs you", async () => {
    const job: NeedsYou = {
      kind: "job",
      worktree: FOREIGN.worktree,
      id: "j-7",
      command: "make test",
      end: { kind: "exited", code: 2 },
    };
    poller(answered(many, { needsYou: [FOREIGN, job] }));

    render(<App store={createDaemonStore()} />);

    const reason = (await entryOf("web")).querySelector(".row-reason");
    expect(reason).toHaveTextContent("make test exited 2");
    expect(reason?.querySelector("svg")).toHaveAttribute("data-glyph", "failure");
  });

  it("gives a Worktree with nothing in Needs you a single line, and its neighbours too", async () => {
    poller(answered(many, { needsYou: [API_FAILURE] }));

    render(<App store={createDaemonStore()} />);

    await entryOf("web-login");
    expect(reasons()).toHaveLength(1);
    const quiet = await entryOf("docs");
    expect(quiet.querySelector(".row-reason")).toBeNull();
    expect(quiet).toHaveTextContent(/^mainMain worktree$/);
  });

  it("draws no reason line at all while nothing needs the developer", async () => {
    poller(answered(many));

    render(<App store={createDaemonStore()} />);

    await entryOf("web-login");
    expect(reasons()).toHaveLength(0);
  });

  it("shows the first item of Needs you for the Worktree, and none of another Worktree's", async () => {
    const second: NeedsYou = {
      kind: "job",
      worktree: "/p/work/acme/web-login",
      id: "j-8",
      command: "make lint",
      end: { kind: "exited", code: 1 },
    };
    const outside: NeedsYou = { ...second, worktree: null, id: "j-9", command: "make outside" };
    poller(answered(many, { needsYou: [outside, API_FAILURE, second, FOREIGN] }));

    render(<App store={createDaemonStore()} />);

    const entry = await entryOf("web-login");
    expect(entry.querySelectorAll(".row-reason")).toHaveLength(1);
    expect(entry.querySelector(".row-reason")).toHaveTextContent("api exited 1");
    // A Job outside every Repository belongs to no Worktree of the sidebar.
    expect(reasons().map((reason) => reason.textContent)).toEqual([
      "a process outside Procly",
      "api exited 1",
    ]);
  });

  it("keeps a long reason whole in one cut-off text, for the stylesheet to end it with an ellipsis", async () => {
    const long: NeedsYou = {
      kind: "job",
      worktree: "/p/work/acme/web-login",
      id: "j-10",
      command: "cargo test --workspace --locked -- --test-threads 1 --nocapture some_very_long_test_name",
      end: { kind: "exited", code: 101 },
    };
    poller(answered(many, { needsYou: [long] }));

    render(<App store={createDaemonStore()} />);

    const reason = (await entryOf("web-login")).querySelector(".row-reason");
    expect(reason?.querySelectorAll(".row-reason-text")).toHaveLength(1);
    expect(reason?.querySelector(".row-reason-text")).toHaveTextContent(`${long.command} exited 101`);
    // One line of its own: the reason is not part of the label's line, which the entry cuts separately.
    expect(reason?.parentElement?.querySelectorAll(".row-line")).toHaveLength(1);
  });

  it("draws no reason line while the Daemon answers badly, since the sidebar gives the window to the state screen", async () => {
    poller(answered(many, { needsYou: [API_FAILURE] }));
    render(<App store={createDaemonStore()} />);
    await entryOf("web-login");

    await tick({
      at_ms: AT,
      failure: { kind: "decode", message: "unreadable answer" },
      needs_you: [],
      seen: [],
    });

    expect(await screen.findByRole("heading", { name: "The Daemon did not answer" })).toBeInTheDocument();
    expect(screen.queryByRole("navigation", { name: "Worktrees" })).not.toBeInTheDocument();
    expect(document.querySelectorAll(".row-reason")).toHaveLength(0);
  });

  it("names the reason in the accessible name of the entry, after the label", async () => {
    poller(answered(many, { needsYou: [API_FAILURE] }));

    render(<App store={createDaemonStore()} />);

    const sidebar = screen.getByRole("navigation", { name: "Worktrees" });
    const entry = await within(sidebar).findByRole("button", { name: new RegExp(`^${LONG_BRANCH}`) });
    expect(entry).toHaveAccessibleName(/api exited 1$/);
  });

  it("follows the poller: the line is gone once the failure is marked as Seen", async () => {
    poller(answered(many, { needsYou: [API_FAILURE] }), () =>
      answered(many, { needsYou: [], seen: [API_KEY] }),
    );
    render(<App store={createDaemonStore()} />);
    expect((await entryOf("web-login")).querySelector(".row-reason")).not.toBeNull();

    const needs = await screen.findByRole("region", { name: "Needs you" });
    fireEvent.click(within(needs).getByRole("button", { name: /^api/ }));
    const panel = await screen.findByRole("region", { name: "Selection" });
    fireEvent.click(within(panel).getByRole("button", { name: "Mark as Seen" }));

    await waitFor(() => {
      expect(reasons()).toHaveLength(0);
    });
    expect((await entryOf("web-login")).querySelector(".row-line")).toHaveTextContent(LONG_BRANCH);
  });

  it("moves to the next item of the Worktree when the first one is marked as Seen", async () => {
    const lint: NeedsYou = {
      kind: "job",
      worktree: "/p/work/acme/web-login",
      id: "j-8",
      command: "make lint",
      end: { kind: "exited", code: 1 },
    };
    poller(answered(many, { needsYou: [API_FAILURE, lint] }));
    render(<App store={createDaemonStore()} />);
    expect((await entryOf("web-login")).querySelector(".row-reason")).toHaveTextContent("api exited 1");

    await tick(answered(many, { needsYou: [lint], seen: [API_KEY] }));

    expect((await entryOf("web-login")).querySelector(".row-reason")).toHaveTextContent("make lint exited 1");
  });
});
