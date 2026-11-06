import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { afterEach, describe, expect, it } from "vitest";

import { App } from "./App";
import { createDaemonStore, type Update } from "./daemon";
import type { DoctorDocument, DoctorState } from "./doctor";
import machine from "./fixtures/doctor.json";
import noDaemon from "./fixtures/doctor-no-daemon.json";
import drift from "./fixtures/doctor-release-drift.json";
import repairs from "./fixtures/doctor-worktree-repairs.json";
import recordedGc from "./fixtures/gc.json";
import recordedStatus from "./fixtures/status.json";
import type { OrphanScan } from "./orphan-scan";
import type { GcReport } from "./protocol/GcReport";
import type { Response } from "./protocol/Response";
import { SidebarFooter } from "./SidebarFooter";

/** A `Gc { apply: false }` answer in the shape the Daemon gives it, 6 Orphans in all. */
const gc = recordedGc as Response as GcReport & { response: "gc" };

/** When the poller last read a Status, and when doctor last ran on the same day. */
const UPDATED = Date.UTC(2026, 10, 5, 14, 1, 12);
const CHECKED = Date.UTC(2026, 10, 5, 13, 40, 7);

function reported(document: object, at = CHECKED): DoctorState {
  return { kind: "report", document: document as DoctorDocument, at };
}

const NEVER_CHECKED: DoctorState = { kind: "idle" };
const NEVER_SCANNED: OrphanScan = { kind: "idle" };

function footer(
  options: {
    updatedAt?: number | null;
    doctor?: DoctorState;
    orphans?: OrphanScan;
  } = {},
) {
  return render(
    <SidebarFooter
      updatedAt={options.updatedAt === undefined ? UPDATED : options.updatedAt}
      doctor={options.doctor ?? reported(machine)}
      orphans={options.orphans ?? { kind: "report", report: gc }}
      checking={false}
      orphansOpen={false}
      settings={false}
      onDoctor={() => undefined}
      onOrphans={() => undefined}
      onSettings={() => undefined}
    />,
  );
}

function entry(name: string): HTMLElement {
  return screen.getByRole("button", { name: new RegExp(`^${name}`) });
}

describe("the Doctor entry of the sidebar footer", () => {
  afterEach(cleanup);

  it("counts the problems of the last run with the absolute time it ran", () => {
    // A recorded run with exactly 2 failed checks.
    footer({ doctor: reported(repairs) });

    expect(entry("Doctor")).toHaveTextContent("2 problems at 13:40");
  });

  it("leaves the warnings out of the count", () => {
    // 1 failed check and 1 warning.
    footer({ doctor: reported(noDaemon) });

    expect(entry("Doctor")).toHaveTextContent("1 problem at 13:40");
  });

  it("says 1 problem in the singular", () => {
    footer({ doctor: reported(drift) });

    expect(entry("Doctor")).toHaveTextContent("1 problem at 13:40");
  });

  it("carries the date of a run that is not of the day of the last update", () => {
    footer({ doctor: reported(repairs, Date.UTC(2026, 10, 4, 13, 40, 7)) });

    expect(entry("Doctor")).toHaveTextContent("2 problems at 2026-11-04 13:40");
  });

  it("shows no count when the last run found no problem", () => {
    const healthy = {
      ...machine,
      checks: machine.checks.filter((check) => check.status === "passed" || check.status === "skipped"),
    };
    footer({ doctor: reported(healthy) });

    expect(entry("Doctor")).toHaveAccessibleName("Doctor");
  });

  it("invents nothing before the first run, nor when it failed", () => {
    const { rerender } = footer({ doctor: NEVER_CHECKED });
    expect(entry("Doctor")).toHaveAccessibleName("Doctor");

    rerender(
      <SidebarFooter
        updatedAt={UPDATED}
        doctor={{ kind: "failure", message: "doctor did not answer" }}
        orphans={NEVER_SCANNED}
        checking={false}
        orphansOpen={false}
        settings={false}
        onDoctor={() => undefined}
        onOrphans={() => undefined}
        onSettings={() => undefined}
      />,
    );
    expect(entry("Doctor")).toHaveAccessibleName("Doctor");
  });

  it("keeps the count of the last run while a new one is on its way", () => {
    footer({ doctor: { kind: "running", document: machine as DoctorDocument, at: CHECKED } });

    expect(entry("Doctor")).toHaveTextContent("3 problems at 13:40");
  });
});

describe("the Orphans entry of the sidebar footer", () => {
  afterEach(cleanup);

  it("counts the Orphans of the last dry run", () => {
    footer();

    expect(entry("Orphans")).toHaveTextContent("6");
  });

  it("shows no count when the dry run found none", () => {
    footer({ orphans: { kind: "report", report: { ...gc, orphans: [] } } });

    expect(entry("Orphans")).toHaveAccessibleName("Orphans");
  });

  it("invents nothing before the first dry run, nor when it failed", () => {
    const { rerender } = footer({ orphans: NEVER_SCANNED });
    expect(entry("Orphans")).toHaveAccessibleName("Orphans");

    rerender(
      <SidebarFooter
        updatedAt={UPDATED}
        doctor={NEVER_CHECKED}
        orphans={{ kind: "failure", message: "the daemon did not answer" }}
        checking={false}
        orphansOpen={false}
        settings={false}
        onDoctor={() => undefined}
        onOrphans={() => undefined}
        onSettings={() => undefined}
      />,
    );
    expect(entry("Orphans")).toHaveAccessibleName("Orphans");
  });

  it("keeps the count of the last dry run while a new one is on its way", () => {
    footer({ orphans: { kind: "running", report: gc } });

    expect(entry("Orphans")).toHaveTextContent("6");
  });
});

describe("the Daemon line of the sidebar footer", () => {
  afterEach(cleanup);

  it("names the version the Daemon runs and the absolute time of the last Status", () => {
    footer();

    expect(screen.getByText("Daemon 0.5.0, updated at 14:01:12")).toBeInTheDocument();
  });

  it("says the Daemon runs another release than this app when it does", () => {
    footer({ doctor: reported(drift) });

    expect(screen.getByText("Daemon 0.4.0, updated at 14:01:12")).toBeInTheDocument();
  });

  it("leaves the version out while doctor has not named one", () => {
    footer({ doctor: NEVER_CHECKED });

    expect(screen.getByText("Daemon, updated at 14:01:12")).toBeInTheDocument();
  });

  it("is a quiet dot beside the line, not a control", () => {
    const { container } = footer();

    const line = screen.getByText("Daemon 0.5.0, updated at 14:01:12").parentElement as HTMLElement;
    expect(line.querySelector('[data-glyph="healthy"]')).not.toBeNull();
    expect(within(container).getAllByRole("button")).toHaveLength(3);
  });

  it("is absent while no Status was read", () => {
    footer({ updatedAt: null });

    expect(screen.queryByText(/^Daemon/)).not.toBeInTheDocument();
  });
});

describe("the sidebar footer", () => {
  afterEach(cleanup);

  it("puts the Daemon line under the Doctor and Orphans counters, and Settings pinned last", () => {
    const { container } = footer();

    const foot = container.firstElementChild as HTMLElement;
    expect([...foot.children].map((child) => child.textContent)).toEqual([
      "Doctor3 problems at 13:40",
      "Orphans6",
      "Daemon 0.5.0, updated at 14:01:12",
      "Settings⌘,",
    ]);
  });

  it("shows no relative age anywhere", () => {
    const { container } = footer();

    expect(container.textContent).not.toMatch(/\bago\b|\bjust now\b|\bseconds?\b|\bminutes?\b|\bhours?\b/i);
  });
});

/** What the Rust side answers: the poller's last Status, doctor for the machine, and the dry run of `Gc`. */
function rust(): string[] {
  const commands: string[] = [];
  mockIPC(
    (command) => {
      commands.push(command);
      switch (command) {
        case "last_update":
          return {
            at_ms: UPDATED,
            response: recordedStatus as Response,
            needs_you: [],
            seen: [],
          } satisfies Update;
        case "doctor":
          return machine;
        case "list_orphans":
          return recordedGc;
        case "refresh":
          return undefined;
        default:
          return Promise.reject(new Error(`unexpected command ${command}`));
      }
    },
    { shouldMockEvents: true },
  );
  return commands;
}

describe("the sidebar footer of the window", () => {
  afterEach(() => {
    cleanup();
    clearMocks();
  });

  it("counts the problems, the Orphans and names the Daemon once the launch scans answered", async () => {
    rust();

    render(<App store={createDaemonStore()} />);

    const sidebar = await screen.findByRole("navigation", { name: "Worktrees" });
    expect(
      await within(sidebar).findByRole("button", {
        name: /^Doctor3 problems at (\d{4}-\d\d-\d\d )?\d\d:\d\d$/,
      }),
    ).toBeVisible();
    expect(within(sidebar).getByRole("button", { name: /^Orphans6$/ })).toBeVisible();
    expect(within(sidebar).getByText("Daemon 0.5.0, updated at 14:01:12")).toBeVisible();
  });

  it("scans again on Refresh, and only then, since nothing runs on a timer", async () => {
    const commands = rust();
    render(<App store={createDaemonStore()} />);
    await screen.findByText("Daemon 0.5.0, updated at 14:01:12");
    expect(commands.filter((command) => command === "list_orphans")).toHaveLength(1);

    fireEvent.click(await screen.findByRole("button", { name: "Refresh" }));

    await waitFor(() => {
      expect(commands.filter((command) => command === "list_orphans")).toHaveLength(2);
    });
  });
});
