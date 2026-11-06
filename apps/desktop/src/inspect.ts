import type { ContainerStatus } from "./protocol/ContainerStatus";
import type { JobEnd } from "./protocol/JobEnd";
import type { JobStatus } from "./protocol/JobStatus";
import type { ServiceEnd } from "./protocol/ServiceEnd";
import type { ServiceStatus } from "./protocol/ServiceStatus";
import { jobWord, serviceWord, type StateWord } from "./state";

/** What the inspector says of the selection: its state, what happened to it in words, and what to do next. */
export interface Statement {
  state: StateWord;
  summary: string;
  next: string;
}

const SIGNALS: Record<number, string> = {
  1: "SIGHUP",
  2: "SIGINT",
  3: "SIGQUIT",
  6: "SIGABRT",
  9: "SIGKILL",
  11: "SIGSEGV",
  13: "SIGPIPE",
  15: "SIGTERM",
};

/** A duration the way a person says it: the largest unit that holds it exactly. */
export function formatDuration(ms: number): string {
  if (ms % 3_600_000 === 0 && ms > 0) {
    return `${String(ms / 3_600_000)} h`;
  }
  if (ms % 60_000 === 0 && ms > 0) {
    return `${String(ms / 60_000)} min`;
  }
  if (ms % 1000 === 0 && ms > 0) {
    return `${String(ms / 1000)} s`;
  }
  return `${String(ms)} ms`;
}

function signalWords(signal: number): string {
  const name = SIGNALS[signal];
  return name === undefined ? `signal ${String(signal)}` : `signal ${String(signal)} (${name})`;
}

const RESTART_AFTER_READING = "Read its output, fix the cause, then restart it.";

function serviceEnd(end: ServiceEnd): Pick<Statement, "summary" | "next"> {
  switch (end.kind) {
    case "exited":
      return end.code === 0
        ? { summary: "Exited with code 0, it finished on its own.", next: "Nothing to do." }
        : {
            summary: `Exited with code ${String(end.code)}.`,
            next: "Read the end of its output, fix the cause, then restart it.",
          };
    case "signaled":
      return {
        summary: `Ended by ${signalWords(end.signal)}.`,
        next: "Find out what sent the signal, then restart it.",
      };
    case "health_timeout":
      return {
        summary: `Did not become healthy within ${formatDuration(end.timeout_ms)}, so the Daemon stopped it.`,
        next: "Read its output to see why it never answered, then restart it.",
      };
    case "stopped":
      return { summary: "Stopped on request.", next: "Nothing to do. Start it again when you need it." };
    case "idle":
      return {
        summary: `Stopped after ${formatDuration(end.timeout_ms)} without output or CPU use.`,
        next: "Nothing to do. Start it again when you need it.",
      };
    case "failed":
      return {
        summary: `Crashed again after ${String(end.restarts)} automatic restarts, so the Daemon gave up.`,
        next: RESTART_AFTER_READING,
      };
    case "port_mismatch":
      return {
        summary: `Listened on port ${String(end.actual)} instead of the port ${String(end.leased)} leased to it, so the Daemon stopped it.`,
        next: "Make it listen on the port Procly gives it, then restart it.",
      };
    case "lost":
      return {
        summary: "Ended while no Daemon was watching, its exit code is unknown.",
        next: "Restart it, and read its output for the cause.",
      };
    case "orphaned":
      return {
        summary: `Its main process had ended when the Daemon restarted, so the Daemon stopped what was left of process group ${String(end.pgid)}${end.port === null ? "" : `, which held port ${String(end.port)}`}.`,
        next: "Restart it.",
      };
    case "not_listening":
      return {
        summary: `Still ran when the Daemon restarted but no longer listened on port ${String(end.port)}, so the Daemon stopped it.`,
        next: "Restart it.",
      };
    default: {
      // An end this app does not know comes from a newer Daemon, and counts as a failure.
      const unknown: never = end;
      return {
        summary: `Ended in a way this app does not know: ${JSON.stringify(unknown)}.`,
        next: RESTART_AFTER_READING,
      };
    }
  }
}

/**
 * What to say of a Service, from its state and, once it ended, how it ended. `failed` says whether that end is a
 * failure, as Rust decided it.
 */
export function serviceStatement(service: ServiceStatus, failed: boolean): Statement {
  const state = serviceWord(service, failed);
  switch (service.state) {
    case "starting":
      return {
        state,
        summary: "Starting. The Daemon waits for its health check to pass.",
        next: "Nothing to do yet.",
      };
    case "healthy":
      return { state, summary: "Running and healthy.", next: "Nothing to do." };
    case "restarting":
      return {
        state,
        summary: "Restarting after a crash. The Daemon starts it again by itself.",
        next: "Read its output to see why it crashed.",
      };
    case "ended":
      return { state, ...serviceEnd(service.end) };
    default: {
      const unknown: never = service;
      return {
        state,
        summary: `In a state this app does not know: ${JSON.stringify(unknown)}.`,
        next: RESTART_AFTER_READING,
      };
    }
  }
}

function jobEnd(end: JobEnd): Pick<Statement, "summary" | "next"> {
  switch (end.kind) {
    case "exited":
      return end.code === 0
        ? { summary: "Exited with code 0.", next: "Nothing to do." }
        : {
            summary: `Exited with code ${String(end.code)}.`,
            next: "Read the end of its output to see what failed.",
          };
    case "signaled":
      return { summary: `Ended by ${signalWords(end.signal)}.`, next: "Find out what sent the signal." };
    case "timed_out":
      return {
        summary: `Stopped by the Daemon after its timeout of ${formatDuration(end.timeout_ms)}.`,
        next: "Read its output to see where it was stuck.",
      };
    default: {
      const unknown: never = end;
      return {
        summary: `Ended in a way this app does not know: ${JSON.stringify(unknown)}.`,
        next: "Read its output.",
      };
    }
  }
}

/** What to say of a Job. `failed` says whether its end is a failure, as Rust decided it. */
export function jobStatement(job: JobStatus, failed: boolean): Statement {
  const state = jobWord(job, failed);
  switch (job.state) {
    case "running":
      return { state, summary: "Running.", next: "Follow its output below." };
    case "ended":
      return { state, ...jobEnd(job.end) };
    default: {
      const unknown: never = job;
      return {
        state,
        summary: `In a state this app does not know: ${JSON.stringify(unknown)}.`,
        next: "Read its output.",
      };
    }
  }
}

/** What to say of an Isolated Compose service: it is listed because it runs. */
export function composeStatement(container: ContainerStatus): Statement {
  return {
    state: { word: "running", tone: "ok", glyph: "healthy" },
    summary: `Running as the Compose service ${container.compose} of the project ${container.project}.`,
    next: "Its output is a snapshot, press Refresh to read it again.",
  };
}
