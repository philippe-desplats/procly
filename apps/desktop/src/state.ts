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

/** A state this app does not know, which a newer Daemon may send. */
function unknownWord(_state: never): string {
  return "unknown";
}

/**
 * The tone of an end. Whether an end is a failure is Rust's to say, through "Needs you" and the Seen set (see
 * `useFailed`), so that the window keeps no copy of the list of routine ends.
 */
function endTone(failed: boolean): Tone {
  return failed ? "fail" : "ended";
}

/** How a Service ended, in a word. */
export function serviceEndText(end: ServiceEnd): string {
  switch (end.kind) {
    case "exited":
      return `exited ${end.code}`;
    case "signaled":
      return `signaled ${end.signal}`;
    case "health_timeout":
      return "health timeout";
    case "stopped":
      return "stopped";
    case "idle":
      return "idle";
    case "failed":
      return "failed";
    case "port_mismatch":
      return "port mismatch";
    case "lost":
      return "lost";
    case "orphaned":
      return "orphaned";
    case "not_listening":
      return "not listening";
    default:
      return unknownWord(end);
  }
}

/** How a Service ended, drawn as a failure when `failed`. */
export function serviceEndWord(end: ServiceEnd, failed: boolean): StateWord {
  return { word: serviceEndText(end), tone: endTone(failed) };
}

function jobEndText(end: JobEnd): string {
  switch (end.kind) {
    case "exited":
      return `exited ${end.code}`;
    case "signaled":
      return `signaled ${end.signal}`;
    case "timed_out":
      return "timed out";
    default:
      return unknownWord(end);
  }
}

/** How a Job ended, drawn as a failure when `failed`. */
export function jobEndWord(end: JobEnd, failed: boolean): StateWord {
  return { word: jobEndText(end), tone: endTone(failed) };
}

/** The state of a Service. `failed` says whether its end, once it ended, is a failure. */
export function serviceWord(service: ServiceStatus, failed: boolean): StateWord {
  switch (service.state) {
    case "starting":
      return { word: "starting", tone: "muted" };
    case "healthy":
      return { word: "healthy", tone: "ok" };
    case "restarting":
      return { word: "restarting", tone: "warn" };
    case "ended":
      return serviceEndWord(service.end, failed);
    default:
      return { word: unknownWord(service), tone: "fail" };
  }
}

/** The state of a Job. `failed` says whether its end, once it ended, is a failure. */
export function jobWord(job: JobStatus, failed: boolean): StateWord {
  switch (job.state) {
    case "running":
      return { word: "running", tone: "ok" };
    case "ended":
      return jobEndWord(job.end, failed);
    default:
      return { word: unknownWord(job), tone: "fail" };
  }
}
