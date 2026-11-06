import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { emit } from "@tauri-apps/api/event";
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { afterEach, describe, expect, it } from "vitest";

import { App } from "./App";
import { createDaemonStore, type FailureKey, type Failure, type NeedsYou, type Update } from "./daemon";
import recorded from "./fixtures/status.json";
import overview from "./fixtures/status-overview.json";
import type { Response } from "./protocol/Response";

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
      return command === "refresh" ? undefined : Promise.reject(new Error(`unexpected command ${command}`));
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
    expect(await within(sidebar).findByRole("button", { name: /^main\s*main$/ })).toBeInTheDocument();
    expect(within(sidebar).getByRole("button", { name: "web-fix" })).toBeInTheDocument();
    expect(within(sidebar).getByRole("heading", { name: "web" })).toBeInTheDocument();
    expect(screen.getByRole("main")).toBeInTheDocument();
    expect(screen.getByRole("complementary", { name: "Inspector" })).toBeInTheDocument();
    expect(commands).toContain("last_update");
  });

  it("never asks the Daemon for anything itself, the poller does, and runs doctor once at launch", async () => {
    const commands = poller(answered(status));

    render(<App store={createDaemonStore()} />);
    await screen.findByRole("button", { name: "web-fix" });

    expect(commands).toEqual(["last_update", "doctor"]);
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
    expect(screen.getByText("/p/work/acme/web-fix")).toBeInTheDocument();
  });

  it("says that no Daemon runs, and sends nothing but the request for the last answer and doctor", async () => {
    const commands = poller(failed("unreachable", "the daemon did not answer on /p/state/procly.sock"));

    render(<App store={createDaemonStore()} />);

    expect(await screen.findByRole("heading", { name: "Daemon not running" })).toBeInTheDocument();
    expect(screen.getByText(/does not start one/)).toBeInTheDocument();
    // Doctor runs when the app enters that state, and it never starts a Daemon.
    expect(commands).toEqual(["last_update", "doctor"]);
  });

  it("shows why a Daemon that does not answer well failed", async () => {
    poller(failed("decode", "unreadable answer from the daemon"));

    render(<App store={createDaemonStore()} />);

    expect(await screen.findByText("unreadable answer from the daemon")).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "Daemon not running" })).not.toBeInTheDocument();
  });

  it("asks the poller for a tick when Refresh is pressed, runs doctor, and does not ask the Daemon", async () => {
    const commands = poller(answered(status));
    render(<App store={createDaemonStore()} />);

    fireEvent.click(await screen.findByRole("button", { name: "Refresh" }));

    await act(async () => {
      await Promise.resolve();
    });
    expect(commands).toEqual(["last_update", "doctor", "refresh", "doctor"]);
  });
});

describe("the Overview", () => {
  afterEach(() => {
    cleanup();
    clearMocks();
  });

  it("is what the window opens on, and lists every Service of every Worktree with its port", async () => {
    poller(answered(many));

    render(<App store={createDaemonStore()} />);

    const overviewPane = await screen.findByRole("region", { name: "Overview" });
    const rows = within(overviewPane).getAllByRole("listitem");
    expect(rows.map((row) => row.textContent)).toEqual([
      "apihealthy:1873119:31:22",
      "workerrestarting19:31:22",
      "apiexited 1:187422026-11-04 19:31:22",
      "sitestarting:1875519:31:40",
      "sitestopped:1876019:30:00",
    ]);
    // A Service without a port shows none instead of a placeholder.
    expect(rows[1]?.textContent).not.toMatch(/:\d{4,5}/);
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
    const rows = within(overviewPane)
      .getAllByRole("listitem")
      .map((row) => row.textContent);
    expect(rows).toEqual([
      "apihealthy:1873119:31:22",
      "workerrestarting19:31:22",
      "dbrunning:24050",
      "apiexited 1:187422026-11-04 19:31:22",
      "sitestarting:1875519:31:40",
      "sitestopped:1876019:30:00",
      "cacherunning",
    ]);
  });

  it("groups the Services under the label of their Worktree", async () => {
    poller(answered(many));

    render(<App store={createDaemonStore()} />);

    const overviewPane = await screen.findByRole("region", { name: "Overview" });
    const headings = within(overviewPane)
      .getAllByRole("heading", { level: 3 })
      .map((heading) => heading.textContent);
    expect(headings).toEqual([
      "main",
      "feature/a-very-long-branch-name-that-the-sidebar-has-to-cut-at-its-end",
      "docs-preview",
      "docs-oldRemoved",
    ]);
  });

  it("shows when each Service started as an absolute time, never as an age, and when the answer was read", async () => {
    poller(answered(many));

    render(<App store={createDaemonStore()} />);

    const overviewPane = await screen.findByRole("region", { name: "Overview" });
    expect(within(overviewPane).queryByText(/\bago\b/)).not.toBeInTheDocument();
    expect(within(overviewPane).getByText("2026-11-04 19:31:22")).toBeInTheDocument();
    expect(screen.getByText("Updated 19:35:00")).toBeInTheDocument();
  });

  it("says so when no Service runs anywhere", async () => {
    poller(answered(status));
    render(<App store={createDaemonStore()} />);
    await screen.findByRole("region", { name: "Overview" });

    await tick(answered({ response: "status", repositories: [], jobs_outside_repositories: [] }));

    expect(await screen.findByText("No Service is known.")).toBeInTheDocument();
  });

  it("comes back with ⌘0 from a Worktree", async () => {
    poller(answered(many));
    render(<App store={createDaemonStore()} />);
    fireEvent.click(await screen.findByRole("button", { name: "docs-preview" }));
    expect(screen.queryByRole("region", { name: "Overview" })).not.toBeInTheDocument();

    fireEvent.keyDown(window, { key: "0", metaKey: true });

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
    await screen.findByRole("heading", { name: BRANCH, level: 2 });

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
          text !== "Outside Repositories" &&
          text !== "Orphans" &&
          text !== "Doctor" &&
          text !== "Settings⌘,",
      );
    expect(labels).toEqual([
      "mainmain",
      "feature/a-very-long-branch-name-that-the-sidebar-has-to-cut-at-its-end",
      "mainmain",
      "docs-preview",
      "docs-oldRemoved",
      "mainmain",
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

  /** The rows of the Overview that are not in "Needs you". */
  function serviceRows(): HTMLElement[] {
    const overviewPane = screen.getByRole("region", { name: "Overview" });
    const needs = within(overviewPane).queryByRole("region", { name: "Needs you" });
    return within(overviewPane)
      .getAllByRole("listitem")
      .filter((row) => needs === null || !needs.contains(row));
  }

  it("is pinned on top of the Overview, with the failures and the Foreign processes it was given", async () => {
    poller(answered(many, { needsYou: [API_FAILURE, FOREIGN] }));

    render(<App store={createDaemonStore()} />);

    const overviewPane = await screen.findByRole("region", { name: "Overview" });
    const needs = within(overviewPane).getByRole("region", { name: "Needs you" });
    expect(within(needs).getByRole("heading", { name: /^Needs you\s*2$/ })).toBeInTheDocument();
    const lines = within(needs)
      .getAllByRole("listitem")
      .map((line) => line.textContent);
    expect(lines).toEqual([
      "apiexited 1feature/a-very-long-branch-name-that-the-sidebar-has-to-cut-at-its-endMark as Seen",
      "Foreignnode server.js:3000main",
    ]);
    const firstService = within(overviewPane).getAllByText("healthy")[0];
    expect(
      needs.compareDocumentPosition(firstService as Node) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
    // A Foreign process is not an ended failure, there is nothing to mark.
    expect(within(needs).getAllByRole("button", { name: "Mark as Seen" })).toHaveLength(1);
  });

  it("draws an ended row as a failure when Rust lists it, unseen or Seen, and as routine otherwise", async () => {
    /** The tone of the state of the ended `api` row of the Overview. */
    const toneOfApi = (): string | undefined =>
      serviceRows()
        .find((row) => row.textContent?.startsWith("apiexited 1"))
        ?.querySelector(".state")
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
    const needs = await screen.findByRole("region", { name: "Needs you" });

    fireEvent.click(within(needs).getByRole("button", { name: "Mark as Seen" }));

    await waitFor(() => {
      expect(within(screen.getByRole("region", { name: "Needs you" })).getAllByRole("listitem")).toHaveLength(
        1,
      );
    });
    expect(marked).toEqual([API_KEY]);
    const left = screen.getByRole("region", { name: "Needs you" });
    expect(within(left).getByRole("heading", { name: /^Needs you\s*1$/ })).toBeInTheDocument();
    expect(within(left).queryByText("exited 1")).not.toBeInTheDocument();
    const row = serviceRows().find((candidate) => candidate.textContent?.startsWith("apiexited 1"));
    expect(row).toBeDefined();
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

    fireEvent.click(await screen.findByRole("button", { name: "Mark as Seen" }));

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

      fireEvent.click(await screen.findByRole("button", { name: "Mark as Seen" }));

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

    fireEvent.click(await screen.findByRole("button", { name: "Mark as Seen" }));
    await waitFor(() => {
      expect(screen.queryByRole("region", { name: "Needs you" })).not.toBeInTheDocument();
    });
    fireEvent.click(screen.getByRole("button", { name: /^main\s*main$/ }));

    expect(marked).toEqual([{ kind: "job", id: "j-7" }]);
    const row = (await screen.findByText("make test")).closest("li");
    expect(row).toHaveAttribute("data-seen", "true");
  });
});
