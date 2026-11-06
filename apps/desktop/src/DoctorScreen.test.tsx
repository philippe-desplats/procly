import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { emit } from "@tauri-apps/api/event";
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { App } from "./App";
import { createDaemonStore, type Failure, type Update } from "./daemon";
import { REFRESH_REQUESTED, type DoctorDocument } from "./doctor";
import machine from "./fixtures/doctor.json";
import noDaemon from "./fixtures/doctor-no-daemon.json";
import drift from "./fixtures/doctor-release-drift.json";
import repairs from "./fixtures/doctor-worktree-repairs.json";
import ofWorktree from "./fixtures/doctor-worktree.json";
import recorded from "./fixtures/status.json";
import type { Response } from "./protocol/Response";

const status: Response = recorded as Response;
const AT = Date.UTC(2026, 10, 5, 19, 35, 0);

const MACHINE: DoctorDocument = machine as DoctorDocument;
const NO_DAEMON: DoctorDocument = noDaemon as DoctorDocument;
const DRIFT: DoctorDocument = drift as DoctorDocument;
const WORKTREE: DoctorDocument = ofWorktree as DoctorDocument;
const REPAIRS: DoctorDocument = repairs as DoctorDocument;

/** The Worktree of the recorded answer the Worktree screen is opened on. */
const WORKTREE_PATH = "/p/work/acme/web-fix";

const NOT_RUNNING: Failure = {
  kind: "unreachable",
  message: "the daemon did not answer on /p/state/procly.sock",
};

function answered(at = AT): Update {
  return { at_ms: at, response: status, needs_you: [], seen: [] };
}

function failed(failure: Failure): Update {
  return { at_ms: AT, failure, needs_you: [], seen: [] };
}

interface Call {
  command: string;
  args: unknown;
}

/**
 * Plays the Rust side. `last` is what the poller already answered, `documents` answers `doctor` by the directory it was
 * asked for (`null` for the machine), and every command the window sends is kept with its arguments.
 */
function rust(last: Update, documents: (cwd: string | null) => DoctorDocument | Error): Call[] {
  const calls: Call[] = [];
  mockIPC(
    (command, args) => {
      calls.push({ command, args });
      if (command === "last_update") {
        return last;
      }
      if (command === "refresh") {
        return undefined;
      }
      if (command === "doctor") {
        const answer = documents((args as { cwd: string | null }).cwd);
        return answer instanceof Error ? Promise.reject({ kind: "doctor", message: answer.message }) : answer;
      }
      return Promise.reject(new Error(`unexpected command ${command}`));
    },
    { shouldMockEvents: true },
  );
  return calls;
}

function doctorCalls(calls: Call[]): unknown[] {
  return calls.filter((call) => call.command === "doctor").map((call) => call.args);
}

async function settle(): Promise<void> {
  await act(async () => {
    await Promise.resolve();
  });
}

/** Records what the Copy buttons put on the clipboard. */
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

/** The names of the checks the list shows, in the order it shows them. */
function listed(list: HTMLElement): string[] {
  return within(list)
    .getAllByRole("listitem")
    .filter((item) => item.dataset.check !== undefined)
    .map((item) => item.dataset.check ?? "");
}

async function openDoctor(): Promise<HTMLElement> {
  fireEvent.click(await screen.findByRole("button", { name: /^Doctor/ }));
  return screen.findByRole("region", { name: "Doctor" });
}

beforeEach(() => {
  clipboard();
});

afterEach(() => {
  cleanup();
  clearMocks();
  vi.useRealTimers();
});

describe("the Doctor screen", () => {
  it("lists the checks of the recorded document, problems first", async () => {
    const calls = rust(answered(), () => MACHINE);
    render(<App store={createDaemonStore()} />);

    const doctor = await openDoctor();

    // Failed first, then the warning, then what passed, then what was skipped: the order of the document within each.
    expect(await within(doctor).findByText("leased_ports")).toBeInTheDocument();
    expect(listed(doctor)).toEqual([
      "leased_ports",
      "orphans",
      "portless_aliases",
      "session_unit",
      "docker_compose",
      "daemon",
      "state_documents",
    ]);
    // Once at launch for the banner, then once for the screen.
    expect(doctorCalls(calls)).toEqual([{ cwd: null }, { cwd: null }]);
  });

  it("leaves out the checks that belong to a Worktree, which a run from no directory only skips", async () => {
    rust(answered(), () => MACHINE);
    render(<App store={createDaemonStore()} />);

    const doctor = await openDoctor();

    await within(doctor).findByText("orphans");
    for (const name of ["configuration", "guard", "portless", "worktrunk_hooks"]) {
      expect(within(doctor).queryByText(name)).not.toBeInTheDocument();
    }
  });

  it("copies exactly the fix each Copy button sits beside", async () => {
    rust(answered(), () => MACHINE);
    const written = clipboard();
    render(<App store={createDaemonStore()} />);
    const doctor = await openDoctor();
    await within(doctor).findByText("orphans");

    const buttons = within(doctor)
      .getAllByRole("button", { name: "Copy" })
      // The prompt has its own button, checked below.
      .filter((button) => button.closest("[data-check]") !== null);
    for (const button of buttons) {
      const shown = button.parentElement?.querySelector("code")?.textContent;
      fireEvent.click(button);
      expect(written.at(-1)).toBe(shown);
    }

    // One command per line: the 2 aliases to remove are 2 buttons, and a line is never merged with the next.
    expect(written).toEqual([
      "kill 4812",
      // The fix for a Port mismatch moves to the Worktree, so it is pasted in parentheses and moves nothing else.
      "(cd /p/work/acme/web-fix && procly env web)",
      "procly gc",
      "portless alias --remove web.acme",
      "portless alias --remove api.acme",
      "procly unit install",
    ]);
  });

  it("shows the prompt to hand to the developer's own agent, and copies it whole", async () => {
    rust(answered(), () => MACHINE);
    const written = clipboard();
    render(<App store={createDaemonStore()} />);
    const doctor = await openDoctor();

    const prompt = await within(doctor).findByRole("group", { name: "Prompt for your agent" });
    fireEvent.click(within(prompt).getByRole("button", { name: "Copy" }));

    expect(written).toEqual([MACHINE.prompt]);
    expect(prompt).toHaveTextContent("kill 4812");
  });

  it("offers no prompt when no check failed", async () => {
    const healthy: DoctorDocument = {
      ...MACHINE,
      result: "healthy",
      prompt: null,
      checks: MACHINE.checks.filter((check) => check.status === "passed"),
    };
    rust(answered(), () => healthy);
    render(<App store={createDaemonStore()} />);

    const doctor = await openDoctor();

    expect(await within(doctor).findByText("docker_compose")).toBeInTheDocument();
    expect(within(doctor).queryByRole("group", { name: "Prompt for your agent" })).not.toBeInTheDocument();
  });

  it("says why doctor could not run, and lets the developer run it again", async () => {
    let failing = true;
    const calls = rust(answered(), () =>
      failing ? new Error("cannot run the embedded procly at /p/app/procly") : MACHINE,
    );
    render(<App store={createDaemonStore()} />);
    const doctor = await openDoctor();

    expect(
      await within(doctor).findByText("cannot run the embedded procly at /p/app/procly"),
    ).toBeInTheDocument();

    failing = false;
    fireEvent.click(screen.getByRole("button", { name: "Refresh" }));
    expect(await within(doctor).findByText("orphans")).toBeInTheDocument();
    expect(doctorCalls(calls)).toHaveLength(3);
  });
});

describe("when doctor runs", () => {
  it("runs once at launch for the banner, then on no screen that does not show it, and on no tick of the poller", async () => {
    const calls = rust(answered(), () => MACHINE);
    render(<App store={createDaemonStore()} />);
    expect(await screen.findByRole("region", { name: "Overview" })).toBeInTheDocument();

    await emit("daemon-update", answered(AT + 2000));
    await settle();

    expect(doctorCalls(calls)).toEqual([{ cwd: null }]);
  });

  it("runs once when the Doctor screen opens, from no directory", async () => {
    const calls = rust(answered(), () => MACHINE);
    render(<App store={createDaemonStore()} />);

    await openDoctor();
    await screen.findByText("orphans");

    // The first is the run at launch.
    expect(doctorCalls(calls)).toEqual([{ cwd: null }, { cwd: null }]);
  });

  it("runs once when a Worktree screen opens, from the directory of that Worktree", async () => {
    const calls = rust(answered(), (cwd) => (cwd === WORKTREE_PATH ? WORKTREE : MACHINE));
    render(<App store={createDaemonStore()} />);

    fireEvent.click(await screen.findByRole("button", { name: /web_fix|web-fix/ }));
    await screen.findByText("configuration");

    expect(doctorCalls(calls)).toEqual([{ cwd: null }, { cwd: WORKTREE_PATH }]);
  });

  it("runs when the app enters Daemon not running, and only then", async () => {
    const calls = rust(answered(), () => NO_DAEMON);
    render(<App store={createDaemonStore()} />);
    expect(await screen.findByRole("region", { name: "Overview" })).toBeInTheDocument();
    // The run at launch.
    expect(doctorCalls(calls)).toEqual([{ cwd: null }]);

    await act(async () => {
      await emit("daemon-update", failed(NOT_RUNNING));
    });
    expect(await screen.findByRole("heading", { name: "Daemon not running" })).toBeInTheDocument();
    expect(doctorCalls(calls)).toEqual([{ cwd: null }, { cwd: null }]);

    // The poller keeps finding no Daemon, which is not entering the state again.
    await act(async () => {
      await emit("daemon-update", failed(NOT_RUNNING));
    });
    expect(doctorCalls(calls)).toEqual([{ cwd: null }, { cwd: null }]);
  });

  it("runs again on Refresh, for the machine and for the Worktree on screen", async () => {
    const calls = rust(answered(), (cwd) => (cwd === WORKTREE_PATH ? WORKTREE : MACHINE));
    render(<App store={createDaemonStore()} />);
    fireEvent.click(await screen.findByRole("button", { name: /web_fix|web-fix/ }));
    await screen.findByText("configuration");

    fireEvent.click(screen.getByRole("button", { name: "Refresh" }));
    await settle();

    expect(doctorCalls(calls)).toEqual([
      { cwd: null },
      { cwd: WORKTREE_PATH },
      { cwd: null },
      { cwd: WORKTREE_PATH },
    ]);
    expect(calls.map((call) => call.command)).toContain("refresh");
  });

  it("runs again when the menu refreshes, since that Refresh does not go through the window", async () => {
    const calls = rust(answered(), () => MACHINE);
    render(<App store={createDaemonStore()} />);
    await openDoctor();
    await screen.findByText("orphans");

    await act(async () => {
      await emit(REFRESH_REQUESTED);
    });
    await settle();

    expect(doctorCalls(calls)).toEqual([{ cwd: null }, { cwd: null }, { cwd: null }]);
  });

  it("installs no timer from the Doctor code, at mount or later, whatever the time that passes", async () => {
    // Every timer is recorded with the place that asked for it, from before the first render. The test tools and React
    // install their own, only those asked for by the files of the Doctor are a failure.
    const doctorFiles = /src\/(doctor\.ts|DoctorScreen\.tsx|CheckList\.tsx|CopyBox\.tsx)/;
    const asked: string[] = [];
    for (const name of [
      "setInterval",
      "setTimeout",
      "requestAnimationFrame",
      "requestIdleCallback",
    ] as const) {
      const original = globalThis[name] as ((...args: unknown[]) => unknown) | undefined;
      if (original === undefined) {
        continue;
      }
      vi.spyOn(globalThis, name as "setTimeout").mockImplementation(((...args: unknown[]) => {
        const stack = new Error().stack ?? "";
        if (doctorFiles.test(stack)) {
          asked.push(`${name}: ${stack}`);
        }
        return original(...args);
      }) as typeof setTimeout);
    }
    const calls = rust(answered(), () => MACHINE);
    render(<App store={createDaemonStore()} />);
    await openDoctor();
    await screen.findByText("orphans");
    fireEvent.click(screen.getByRole("button", { name: "Refresh" }));
    await settle();
    const before = doctorCalls(calls).length;

    vi.restoreAllMocks();
    vi.useFakeTimers({ toFake: ["setInterval", "setTimeout"] });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(60 * 60 * 1000);
    });

    expect(asked).toEqual([]);
    expect(doctorCalls(calls)).toHaveLength(before);
  });
});

describe("the Worktree screen", () => {
  it("shows the doctor checks of that Worktree, and none of the machine", async () => {
    rust(answered(), (cwd) => (cwd === WORKTREE_PATH ? WORKTREE : MACHINE));
    render(<App store={createDaemonStore()} />);

    fireEvent.click(await screen.findByRole("button", { name: /web_fix|web-fix/ }));

    const checks = await screen.findByRole("region", { name: "Doctor checks" });
    expect(await within(checks).findByText("configuration")).toBeInTheDocument();
    expect(listed(checks)).toEqual(["configuration", "worktrunk_hooks", "guard", "portless"]);
    expect(within(checks).getByText(/unknown key `servcies`/)).toBeInTheDocument();
    expect(within(checks).queryByText("docker_compose")).not.toBeInTheDocument();
  });

  it("copies the fix of a check of the Worktree", async () => {
    rust(answered(), () => WORKTREE);
    const written = clipboard();
    render(<App store={createDaemonStore()} />);
    fireEvent.click(await screen.findByRole("button", { name: /web_fix|web-fix/ }));
    const checks = await screen.findByRole("region", { name: "Doctor checks" });
    await within(checks).findByText("configuration");

    fireEvent.click(within(checks).getByRole("button", { name: "Copy" }));

    expect(written).toEqual(["${EDITOR:-vi} /p/work/acme/web-fix/procly.toml"]);
  });

  it("copies a command that needs the Worktree as (cd <path> && procly <verb>), shown as it is copied", async () => {
    rust(answered(), (cwd) => (cwd === WORKTREE_PATH ? REPAIRS : MACHINE));
    const written = clipboard();
    render(<App store={createDaemonStore()} />);
    fireEvent.click(await screen.findByRole("button", { name: /web_fix|web-fix/ }));
    const checks = await screen.findByRole("region", { name: "Doctor checks" });
    await within(checks).findByText("guard");

    const shown: string[] = [];
    for (const button of within(checks).getAllByRole("button", { name: "Copy" })) {
      shown.push(button.parentElement?.querySelector("code")?.textContent ?? "");
      fireEvent.click(button);
    }

    const expected = [
      "(cd /p/work/acme/web-fix && procly agent install)",
      "(cd /p/work/acme/web-fix && procly worktree hooks)",
    ];
    expect(shown).toEqual(expected);
    expect(written).toEqual(expected);
  });
});

describe("Daemon not running", () => {
  it("shows the checks that run without the Daemon once doctor ran on entering the state", async () => {
    rust(failed(NOT_RUNNING), () => NO_DAEMON);
    render(<App store={createDaemonStore()} />);

    const checks = await screen.findByRole("region", { name: "Checks without the Daemon" });

    expect(await within(checks).findByText("session_unit")).toBeInTheDocument();
    expect(listed(checks)).toEqual([
      "daemon",
      "session_unit",
      "docker_compose",
      "state_documents",
      "configuration",
      "guard",
      "portless",
      "worktrunk_hooks",
    ]);
    // The 3 checks that need the Daemon were skipped for want of one: they say nothing the screen does not.
    for (const name of ["leased_ports", "orphans", "portless_aliases"]) {
      expect(within(checks).queryByText(name)).not.toBeInTheDocument();
    }
    expect(screen.getByRole("heading", { name: "Daemon not running" })).toBeInTheDocument();
  });

  it("runs doctor on entering the state, and copies the fix of a check", async () => {
    const calls = rust(failed(NOT_RUNNING), () => NO_DAEMON);
    const written = clipboard();
    render(<App store={createDaemonStore()} />);
    const checks = await screen.findByRole("region", { name: "Checks without the Daemon" });
    await within(checks).findByText("session_unit");

    for (const button of within(checks).getAllByRole("button", { name: "Copy" })) {
      fireEvent.click(button);
    }

    expect(doctorCalls(calls)).toEqual([{ cwd: null }]);
    expect(written).toEqual(["procly status", "procly unit install"]);
  });

  it("shows nothing of the report kept from before the Daemon stopped while doctor runs again", async () => {
    let answer: (document: DoctorDocument) => void = () => undefined;
    let asked = 0;
    mockIPC(
      (command) => {
        if (command === "last_update") {
          return answered();
        }
        if (command === "doctor") {
          asked += 1;
          // The first run finds the Daemon, the second one waits for the test to answer it.
          return asked === 1
            ? MACHINE
            : new Promise<DoctorDocument>((resolve) => {
                answer = resolve;
              });
        }
        return Promise.reject(new Error(`unexpected command ${command}`));
      },
      { shouldMockEvents: true },
    );
    render(<App store={createDaemonStore()} />);
    await openDoctor();
    await screen.findByText("orphans");
    fireEvent.click(screen.getByRole("button", { name: /^Overview/ }));

    await act(async () => {
      await emit("daemon-update", failed(NOT_RUNNING));
    });
    const checks = await screen.findByRole("region", { name: "Checks without the Daemon" });
    expect(within(checks).getByText("Checking…")).toBeInTheDocument();
    expect(within(checks).queryByText("the Daemon answers and runs procly 0.5.0")).not.toBeInTheDocument();

    await act(async () => {
      answer(NO_DAEMON);
      await Promise.resolve();
    });
    expect(await within(checks).findByText(/no Daemon runs/)).toBeInTheDocument();
  });

  it("still lets the developer start the Daemon when doctor cannot run", async () => {
    rust(failed(NOT_RUNNING), () => new Error("cannot run the embedded procly at /p/app/procly"));
    render(<App store={createDaemonStore()} />);

    expect(await screen.findByText("cannot run the embedded procly at /p/app/procly")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Start the Daemon" })).toBeEnabled();
  });
});

describe("a Daemon of another release", () => {
  it("shows a banner from doctor at launch, without a Refresh or a visit to Doctor, and blocks nothing", async () => {
    const calls = rust(answered(), () => DRIFT);
    render(<App store={createDaemonStore()} />);

    const banner = await screen.findByRole("status");
    // Once at launch, on no timer: the machine checks, and only them.
    expect(doctorCalls(calls)).toEqual([{ cwd: null }]);
    expect(banner).toHaveTextContent("the Daemon runs procly 0.4.0, this is procly 0.5.0");
    // Everything stays usable: the Overview is still there, and so is every button.
    expect(screen.getByRole("region", { name: "Overview" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Refresh" })).toBeEnabled();
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });

  it("sends the developer to the Doctor screen from the banner", async () => {
    rust(answered(), () => DRIFT);
    render(<App store={createDaemonStore()} />);
    fireEvent.click(await screen.findByRole("button", { name: "Refresh" }));
    const banner = await screen.findByRole("status");

    fireEvent.click(within(banner).getByRole("button", { name: "Open Doctor" }));

    expect(await screen.findByRole("region", { name: "Doctor" })).toBeInTheDocument();
  });

  it("shows no banner for a report that finds no Daemon, nor for one that finds the same release", async () => {
    rust(answered(), () => MACHINE);
    render(<App store={createDaemonStore()} />);
    fireEvent.click(await screen.findByRole("button", { name: "Refresh" }));
    await settle();
    expect(screen.queryByRole("status")).not.toBeInTheDocument();

    cleanup();
    clearMocks();
    rust(answered(), () => NO_DAEMON);
    render(<App store={createDaemonStore()} />);
    fireEvent.click(await screen.findByRole("button", { name: "Refresh" }));
    await settle();
    expect(screen.queryByRole("status")).not.toBeInTheDocument();
  });

  it("shows no banner on the blocking screen of another protocol version, which says it already", async () => {
    rust(
      failed({
        kind: "protocol_mismatch",
        message: "protocol version 21 is not supported, this procly speaks version 22",
        daemon_protocol: 21,
        app_protocol: 22,
      }),
      () => DRIFT,
    );
    render(<App store={createDaemonStore()} />);

    expect(await screen.findByRole("heading", { name: "Daemon of another version" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Refresh" }));
    await settle();

    expect(screen.queryByRole("status")).not.toBeInTheDocument();
  });
});
