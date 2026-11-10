import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { afterEach, describe, expect, it } from "vitest";

import { App, overviewIsEmpty } from "./App";
import { createDaemonStore, type NeedsYou, type Update } from "./daemon";
import { ItemRow } from "./ItemRow";
import { PortPill } from "./PortPill";
import type { Response } from "./protocol/Response";
import { SelectionContext } from "./selectable";
import { foreignWord, serviceEndWord } from "./state";

// What the ticket "bring the remaining minor polish of the desktop app to its mockup" changed in behaviour or in
// structure, one test for each, each of which fails without the change.

const AT = Date.UTC(2026, 10, 7, 21, 0, 0);
const PATH = "/p/work/acme/web-fix";

const FOREIGN: NeedsYou = {
  kind: "foreign",
  worktree: PATH,
  process: { kind: "process", pid: 4812, command: "node server.js", ports: [3000] },
};

function status(overrides: Partial<Extract<Response, { response: "status" }>> = {}): Response {
  return {
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
            ],
            jobs: [],
            foreign: [
              FOREIGN.kind === "foreign"
                ? FOREIGN.process
                : { kind: "process", pid: 1, command: "x", ports: [] },
            ],
            containers: [],
          },
        ],
      },
    ],
    jobs_outside_repositories: [],
    ...overrides,
  } as Response;
}

function rust(update: Update): void {
  mockIPC(
    (command) => {
      switch (command) {
        case "last_update":
          return update;
        case "follow_logs":
          return new Promise<void>(() => undefined);
        case "refresh":
        case "stop_follow":
        case "set_menu_context":
        case "show_context_menu":
          return undefined;
        default:
          return Promise.reject(new Error(`unexpected command ${command}`));
      }
    },
    { shouldMockEvents: true },
  );
}

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

afterEach(() => {
  cleanup();
  clearMocks();
});

describe("the port pill", () => {
  it("is a button that copies the port, named for what it does", () => {
    const written = clipboard();
    render(<PortPill port={18731} />);

    fireEvent.click(screen.getByRole("button", { name: "Copy port 18731" }));

    expect(written).toEqual(["18731"]);
  });

  it("sits beside the button that picks its row, never inside it, and copying does not pick the row", () => {
    const written = clipboard();
    const picked: unknown[] = [];
    render(
      <SelectionContext.Provider
        value={{
          selection: null,
          select: (target) => picked.push(target),
          showMenu: () => undefined,
        }}
      >
        <ul>
          <ItemRow
            target={{ kind: "service", worktree: PATH, service: "web" }}
            seen={false}
            state={{ word: "healthy", tone: "ok", glyph: "healthy" }}
            name="web"
            ports={[18731]}
          />
        </ul>
      </SelectionContext.Provider>,
    );
    const pill = screen.getByRole("button", { name: "Copy port 18731" });
    const row = screen.getByRole("button", { name: /^web/ });

    expect(row).not.toContainElement(pill);
    fireEvent.click(pill);

    expect(written).toEqual(["18731"]);
    expect(picked).toEqual([]);
  });
});

describe("a row of the center", () => {
  it("appends ', seen' to the word of its state, with no separate mark", () => {
    render(
      <ul>
        <ItemRow
          target={{ kind: "service", worktree: PATH, service: "api" }}
          seen
          state={serviceEndWord({ kind: "exited", code: 1 }, true)}
          name="api"
        />
      </ul>,
    );

    expect(screen.getByText("exited 1, seen")).toHaveClass("word");
    expect(screen.queryByText("Seen")).toBeNull();
  });

  it("gives a Foreign process of Needs you its pid after its name", async () => {
    rust({ at_ms: AT, response: status(), needs_you: [FOREIGN], seen: [] });
    render(<App store={createDaemonStore()} />);

    const needs = await screen.findByRole("region", { name: "Needs you" });

    const row = within(needs).getByRole("listitem");
    expect(row).toHaveTextContent("node server.js pid 4812");
    expect(within(row).getByText("pid 4812")).toHaveClass("caption-whole");
    expect(row.querySelector(".word")).toHaveTextContent(foreignWord().word);
  });
});

describe("the Overview", () => {
  it("titles Needs you without a count, which the sidebar already gives", async () => {
    rust({ at_ms: AT, response: status(), needs_you: [FOREIGN], seen: [] });
    render(<App store={createDaemonStore()} />);

    const needs = await screen.findByRole("region", { name: "Needs you" });

    expect(within(needs).getByRole("heading")).toHaveTextContent(/^Needs you$/);
  });

  it("holds no inspector while it has nothing to list, and has one again as soon as something runs", async () => {
    const empty = status({
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
              services: [],
              jobs: [],
              foreign: [],
              containers: [],
            },
          ],
        },
      ],
    });
    rust({ at_ms: AT, response: empty, needs_you: [], seen: [] });
    const { unmount } = render(<App store={createDaemonStore()} />);

    expect(await screen.findByText("Nothing is running yet")).toBeInTheDocument();
    expect(screen.queryByRole("complementary", { name: "Inspector" })).toBeNull();
    unmount();
    cleanup();
    clearMocks();

    rust({ at_ms: AT, response: status(), needs_you: [], seen: [] });
    render(<App store={createDaemonStore()} />);
    await screen.findByRole("region", { name: "Overview" });
    expect(screen.getByRole("complementary", { name: "Inspector" })).toBeInTheDocument();
  });

  it("is empty when no line of Needs you, no Service and no Isolated Compose service is anywhere", () => {
    const base = status();
    if (base.response !== "status") {
      throw new Error("a status");
    }
    const worktrees = base.repositories.flatMap((repository) => repository.worktrees);
    const first = worktrees[0];
    if (first === undefined) {
      throw new Error("a Worktree");
    }

    expect(overviewIsEmpty([], worktrees)).toBe(false);
    expect(overviewIsEmpty([FOREIGN], [{ ...first, services: [] }])).toBe(false);
    expect(overviewIsEmpty([], [{ ...first, services: [] }])).toBe(true);
  });
});

describe("the inspector of a Service, a Job and a Compose service", () => {
  it("sets its title block apart with a rule, as the inspector of a Foreign process does", async () => {
    rust({ at_ms: AT, response: status(), needs_you: [], seen: [] });
    render(<App store={createDaemonStore()} />);
    const overview = await screen.findByRole("region", { name: "Overview" });

    fireEvent.click(within(overview).getByRole("button", { name: /^web/ }));

    const panel = await screen.findByRole("region", { name: "Selection" });
    const title = within(panel).getByRole("heading", { name: "web" });
    expect(title.closest("header")).toHaveClass("inspector-head");
  });

  it("lists its facts as a grid of a label and its value, so that the output keeps the room", async () => {
    rust({ at_ms: AT, response: status(), needs_you: [], seen: [] });
    render(<App store={createDaemonStore()} />);
    const overview = await screen.findByRole("region", { name: "Overview" });

    fireEvent.click(within(overview).getByRole("button", { name: /^web/ }));

    const panel = await screen.findByRole("region", { name: "Selection" });
    const facts = panel.querySelector("dl.facts");
    expect(facts).toHaveClass("facts-grid");
    expect(within(panel).getByText("Started").className).not.toContain("caption");
  });
});
