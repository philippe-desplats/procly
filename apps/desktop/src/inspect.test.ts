import { describe, expect, it } from "vitest";

import { composeStatement, formatDuration, jobStatement, serviceStatement } from "./inspect";
import type { ContainerStatus } from "./protocol/ContainerStatus";
import type { JobEnd } from "./protocol/JobEnd";
import type { JobStatus } from "./protocol/JobStatus";
import type { ServiceEnd } from "./protocol/ServiceEnd";
import type { ServiceStatus } from "./protocol/ServiceStatus";

function service(state: "starting" | "healthy" | "restarting"): ServiceStatus {
  return { service: "api", port: 24001, reattached: false, started_at: 0, restartable: true, state };
}

function ended(end: ServiceEnd): ServiceStatus {
  return {
    service: "api",
    port: 24001,
    reattached: false,
    started_at: 0,
    restartable: true,
    state: "ended",
    end,
  };
}

function job(state: JobStatus["state"], end?: JobEnd): JobStatus {
  return {
    id: "3f9a1c2e",
    command: "bun test",
    cwd: "/p/work/acme/web",
    state,
    ...(end ? { end } : {}),
  } as JobStatus;
}

describe("a duration in words", () => {
  it.each([
    [250, "250 ms"],
    [1000, "1 s"],
    [90_000, "90 s"],
    [60_000, "1 min"],
    [1_800_000, "30 min"],
    [3_600_000, "1 h"],
    [7_200_000, "2 h"],
  ])("%i ms is %s", (ms, words) => {
    expect(formatDuration(ms)).toBe(words);
  });
});

describe("what the inspector says of a Service", () => {
  it.each<[string, ServiceStatus, string, string, string]>([
    [
      "starting",
      service("starting"),
      "starting",
      "Starting. The Daemon waits for its health check to pass.",
      "Nothing to do yet.",
    ],
    ["healthy", service("healthy"), "healthy", "Running and healthy.", "Nothing to do."],
    [
      "restarting",
      service("restarting"),
      "restarting",
      "Restarting after a crash. The Daemon starts it again by itself.",
      "Read its output to see why it crashed.",
    ],
    [
      "exited with 0",
      ended({ kind: "exited", code: 0 }),
      "exited 0",
      "Exited with code 0, it finished on its own.",
      "Nothing to do.",
    ],
    [
      "exited with an error",
      ended({ kind: "exited", code: 3 }),
      "exited 3",
      "Exited with code 3.",
      "Read the end of its output, fix the cause, then restart it.",
    ],
    [
      "signaled",
      ended({ kind: "signaled", signal: 9 }),
      "signaled 9",
      "Ended by signal 9 (SIGKILL).",
      "Find out what sent the signal, then restart it.",
    ],
    [
      "an unnamed signal",
      ended({ kind: "signaled", signal: 40 }),
      "signaled 40",
      "Ended by signal 40.",
      "Find out what sent the signal, then restart it.",
    ],
    [
      "health timeout",
      ended({ kind: "health_timeout", timeout_ms: 60_000 }),
      "health timeout",
      "Did not become healthy within 1 min, so the Daemon stopped it.",
      "Read its output to see why it never answered, then restart it.",
    ],
    [
      "stopped",
      ended({ kind: "stopped" }),
      "stopped",
      "Stopped on request.",
      "Nothing to do. Start it again when you need it.",
    ],
    [
      "idle",
      ended({ kind: "idle", timeout_ms: 1_800_000 }),
      "idle",
      "Stopped after 30 min without output or CPU use.",
      "Nothing to do. Start it again when you need it.",
    ],
    [
      "failed",
      ended({ kind: "failed", restarts: 3 }),
      "failed",
      "Crashed again after 3 automatic restarts, so the Daemon gave up.",
      "Read its output, fix the cause, then restart it.",
    ],
    [
      "port mismatch",
      ended({ kind: "port_mismatch", leased: 24001, actual: 3001 }),
      "port mismatch",
      "Listened on port 3001 instead of the port 24001 leased to it, so the Daemon stopped it.",
      "Make it listen on the port Procly gives it, then restart it.",
    ],
    [
      "lost",
      ended({ kind: "lost" }),
      "lost",
      "Ended while no Daemon was watching, its exit code is unknown.",
      "Restart it, and read its output for the cause.",
    ],
    [
      "orphaned with a port",
      ended({ kind: "orphaned", pgid: 4242, port: 24001 }),
      "orphaned",
      "Its main process had ended when the Daemon restarted, so the Daemon stopped what was left of process group 4242, which held port 24001.",
      "Restart it.",
    ],
    [
      "orphaned without a port",
      ended({ kind: "orphaned", pgid: 4242, port: null }),
      "orphaned",
      "Its main process had ended when the Daemon restarted, so the Daemon stopped what was left of process group 4242.",
      "Restart it.",
    ],
    [
      "not listening",
      ended({ kind: "not_listening", port: 24001 }),
      "not listening",
      "Still ran when the Daemon restarted but no longer listened on port 24001, so the Daemon stopped it.",
      "Restart it.",
    ],
  ])("%s", (_name, status, word, summary, next) => {
    const statement = serviceStatement(status);

    expect(statement.state.word).toBe(word);
    expect(statement.summary).toBe(summary);
    expect(statement.next).toBe(next);
  });

  it("calls a failure a failure and a routine end routine", () => {
    expect(serviceStatement(ended({ kind: "exited", code: 3 })).state.tone).toBe("fail");
    expect(serviceStatement(ended({ kind: "stopped" })).state.tone).toBe("ended");
  });
});

describe("what the inspector says of a Job", () => {
  it.each<[string, JobStatus, string, string, string]>([
    ["running", job("running"), "running", "Running.", "Follow its output below."],
    [
      "exited with 0",
      job("ended", { kind: "exited", code: 0 }),
      "exited 0",
      "Exited with code 0.",
      "Nothing to do.",
    ],
    [
      "exited with an error",
      job("ended", { kind: "exited", code: 1 }),
      "exited 1",
      "Exited with code 1.",
      "Read the end of its output to see what failed.",
    ],
    [
      "signaled",
      job("ended", { kind: "signaled", signal: 15 }),
      "signaled 15",
      "Ended by signal 15 (SIGTERM).",
      "Find out what sent the signal.",
    ],
    [
      "timed out",
      job("ended", { kind: "timed_out", timeout_ms: 30_000 }),
      "timed out",
      "Stopped by the Daemon after its timeout of 30 s.",
      "Read its output to see where it was stuck.",
    ],
  ])("%s", (_name, status, word, summary, next) => {
    const statement = jobStatement(status);

    expect(statement.state.word).toBe(word);
    expect(statement.summary).toBe(summary);
    expect(statement.next).toBe(next);
  });
});

describe("what the inspector says of a Compose service", () => {
  it("says it runs, and that its output is read again on request", () => {
    const container: ContainerStatus = {
      service: "db",
      project: "web-fix",
      compose: "postgres",
      port: 24002,
    };

    const statement = composeStatement(container);

    expect(statement.summary).toBe("Running as the Compose service postgres of the project web-fix.");
    expect(statement.next).toBe("Its output is a snapshot, press Refresh to read it again.");
  });
});
