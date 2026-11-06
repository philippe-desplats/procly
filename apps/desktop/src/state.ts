import type { JobEnd } from "./protocol/JobEnd";
import type { JobStatus } from "./protocol/JobStatus";
import type { ServiceEnd } from "./protocol/ServiceEnd";
import type { ServiceStatus } from "./protocol/ServiceStatus";

/** The colour a state is drawn in, which is the only thing colour carries (a token of `styles.css`). */
export type Tone = "ok" | "warn" | "fail" | "ended" | "muted";

/**
 * The shape a state is drawn in, which is what tells it apart for a person who cannot tell the colours apart. The 6
 * states of a Service, a Job and a Foreign process come first, then the 3 shapes only the checks of the Doctor use.
 */
export type Glyph =
  | "healthy"
  | "ended"
  | "busy"
  | "failure"
  | "job"
  | "foreign"
  | "passed"
  | "skipped"
  | "warning";

export interface StateWord {
  word: string;
  tone: Tone;
  glyph: Glyph;
}

/** A state this app does not know, which a newer Daemon may send. */
function unknownWord(_state: never): string {
  return "unknown";
}

/**
 * How an end is drawn. Whether an end is a failure is Rust's to say, through "Needs you" and the Seen set (see
 * `useFailed`), so that the window keeps no copy of the list of routine ends.
 */
function endLook(failed: boolean): Pick<StateWord, "tone" | "glyph"> {
  return failed ? { tone: "fail", glyph: "failure" } : { tone: "ended", glyph: "ended" };
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
  return { word: serviceEndText(end), ...endLook(failed) };
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
  return { word: jobEndText(end), ...endLook(failed) };
}

/** The state of a Service. `failed` says whether its end, once it ended, is a failure. */
export function serviceWord(service: ServiceStatus, failed: boolean): StateWord {
  switch (service.state) {
    case "starting":
      return { word: "starting", tone: "muted", glyph: "busy" };
    case "healthy":
      return { word: "healthy", tone: "ok", glyph: "healthy" };
    case "restarting":
      return { word: "restarting", tone: "warn", glyph: "busy" };
    case "ended":
      return serviceEndWord(service.end, failed);
    default:
      return { word: unknownWord(service), tone: "fail", glyph: "failure" };
  }
}

/** The state of a Job. `failed` says whether its end, once it ended, is a failure. */
export function jobWord(job: JobStatus, failed: boolean): StateWord {
  switch (job.state) {
    case "running":
      return { word: "running", tone: "muted", glyph: "job" };
    case "ended":
      return jobEndWord(job.end, failed);
    default:
      return { word: unknownWord(job), tone: "fail", glyph: "failure" };
  }
}

/** A process Procly did not start. It is not a failure, so it is amber and a diamond, never red. */
export function foreignWord(): StateWord {
  return { word: "started outside Procly", tone: "warn", glyph: "foreign" };
}
