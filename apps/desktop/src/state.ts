import type { JobEnd } from "./protocol/JobEnd";
import type { JobStatus } from "./protocol/JobStatus";
import type { ServiceEnd } from "./protocol/ServiceEnd";
import type { ServiceStatus } from "./protocol/ServiceStatus";

/** The colour a state is drawn in, which is the only thing colour carries (a token of `styles.css`). */
export type Tone = "ok" | "warn" | "fail" | "ended" | "muted";

export interface StateWord {
  word: string;
  tone: Tone;
}

/** A state this app does not know, which a newer Daemon may send: it counts as a failure, as the spec says. */
function unknownState(_state: never): StateWord {
  return { word: "unknown", tone: "fail" };
}

export function serviceEndWord(end: ServiceEnd): StateWord {
  switch (end.kind) {
    case "exited":
      return { word: `exited ${end.code}`, tone: end.code === 0 ? "ended" : "fail" };
    case "signaled":
      return { word: `signaled ${end.signal}`, tone: "fail" };
    case "health_timeout":
      return { word: "health timeout", tone: "fail" };
    case "stopped":
      return { word: "stopped", tone: "ended" };
    case "idle":
      return { word: "idle", tone: "ended" };
    case "failed":
      return { word: "failed", tone: "fail" };
    case "port_mismatch":
      return { word: "port mismatch", tone: "fail" };
    case "lost":
      return { word: "lost", tone: "fail" };
    case "orphaned":
      return { word: "orphaned", tone: "fail" };
    case "not_listening":
      return { word: "not listening", tone: "fail" };
    default:
      return unknownState(end);
  }
}

export function jobEndWord(end: JobEnd): StateWord {
  switch (end.kind) {
    case "exited":
      return { word: `exited ${end.code}`, tone: end.code === 0 ? "ended" : "fail" };
    case "signaled":
      return { word: `signaled ${end.signal}`, tone: "fail" };
    case "timed_out":
      return { word: "timed out", tone: "fail" };
    default:
      return unknownState(end);
  }
}

export function serviceWord(service: ServiceStatus): StateWord {
  switch (service.state) {
    case "starting":
      return { word: "starting", tone: "muted" };
    case "healthy":
      return { word: "healthy", tone: "ok" };
    case "restarting":
      return { word: "restarting", tone: "warn" };
    case "ended":
      return serviceEndWord(service.end);
    default:
      return unknownState(service);
  }
}

export function jobWord(job: JobStatus): StateWord {
  switch (job.state) {
    case "running":
      return { word: "running", tone: "ok" };
    case "ended":
      return jobEndWord(job.end);
    default:
      return unknownState(job);
  }
}
