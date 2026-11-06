import { Channel, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useCallback, useEffect, useRef, useState } from "react";

import { failureMessage } from "./daemon";
import type { Response } from "./protocol/Response";
import { Transcript, type Line } from "./transcript";

/** The event Rust sends when the window is hidden (`false`) or visible again (`true`). */
export const WINDOW_VISIBLE = "window-visible";

/** How long a burst of output waits before it is drawn, so that a fast Service costs a few redraws a second. */
const REDRAW_MS = 50;

/**
 * Whose output the inspector shows. A Service or a Job is followed live. A Compose service cannot be, so its output is
 * a snapshot the developer reads again with Refresh. `running` is false once the item has ended.
 */
export type OutputTarget =
  | { mode: "follow"; name: string; cwd: string; running: boolean }
  | { mode: "snapshot"; name: string; cwd: string };

/** Where the output stands. */
export type OutputStatus =
  /** Asked for, nothing arrived yet. */
  | "connecting"
  | "following"
  /** The Service or the Job ended and its last line arrived. */
  | "ended"
  /** The window was hidden: the Daemon was told to stop sending, and it starts again when the window comes back. */
  | "paused"
  /** The Daemon refused, or could not be reached: `message` says why. */
  | "failed"
  | "snapshot";

export interface OutputView {
  lines: Line[];
  status: OutputStatus;
  message: string | null;
  /** When a snapshot was read, in milliseconds since the Unix epoch. */
  at: number | null;
  /** Reads a snapshot again. A followed item needs no refresh. */
  refresh: () => void;
}

type Shown = Omit<OutputView, "refresh">;

const NOTHING: Shown = { lines: [], status: "connecting", message: null, at: null };

/** The command numbers, in the order the window sends them. Rust ignores one that arrives after a newer one. */
let tickets = 0;

function nextTicket(): number {
  tickets += 1;
  return tickets;
}

/**
 * The output of `target`, followed from a fresh backlog whenever the item changes, the window comes back, or an ended
 * Service runs again. The follow is closed when the item changes, the inspector shows something else, or the window
 * is hidden. It is keyed on the names of the item and never on its state, so a new answer of the poller does not
 * restart it.
 */
export function useOutput(target: OutputTarget | null): OutputView {
  const [shown, setShown] = useState<Shown>(NOTHING);
  // Bumped to start over: the window came back, an ended Service runs again, or Refresh was pressed.
  const [round, setRound] = useState(0);
  const mode = target?.mode ?? null;
  const name = target?.name ?? "";
  const cwd = target?.cwd ?? "";
  const running = target?.mode === "follow" && target.running;

  const wasRunning = useRef(running);
  useEffect(() => {
    if (!wasRunning.current && running) {
      setRound((current) => current + 1);
    }
    wasRunning.current = running;
  }, [running]);

  useEffect(() => {
    let stopped = false;
    let unlisten: (() => void) | null = null;
    void listen<boolean>(WINDOW_VISIBLE, (event) => {
      if (event.payload && !stopped) {
        setRound((current) => current + 1);
      }
    }).then((off) => {
      if (stopped) {
        off();
      } else {
        unlisten = off;
      }
    });
    return () => {
      stopped = true;
      unlisten?.();
    };
  }, []);

  useEffect(() => {
    if (mode === null) {
      return undefined;
    }
    return mode === "follow" ? follow(name, cwd, setShown) : snapshot(name, cwd, setShown);
  }, [mode, name, cwd, round]);

  const refresh = useCallback(() => {
    setRound((current) => current + 1);
  }, []);
  return { ...(mode === null ? NOTHING : shown), refresh };
}

/** Follows `name` until the returned function is called. */
function follow(name: string, cwd: string, show: (shown: Shown) => void): () => void {
  const transcript = new Transcript();
  const ticket = nextTicket();
  let live = true;
  let status: OutputStatus = "connecting";
  let message: string | null = null;
  let timer: ReturnType<typeof setTimeout> | undefined;

  const draw = (): void => {
    timer = undefined;
    show({ lines: transcript.lines(), status, message, at: null });
  };
  const drawSoon = (): void => {
    timer ??= setTimeout(draw, REDRAW_MS);
  };
  const end = (next: OutputStatus, why: string | null): void => {
    status = next;
    message = why;
    clearTimeout(timer);
    draw();
  };

  // A channel of its own for each follow, so that what an earlier one still writes is told apart and dropped.
  const channel = new Channel<Response>();
  channel.onmessage = (response) => {
    if (!live) {
      return;
    }
    switch (response.response) {
      case "logs":
        transcript.write(response.output);
        status = "following";
        drawSoon();
        break;
      case "output":
        transcript.write(response.text);
        drawSoon();
        break;
      case "output_dropped":
        transcript.skipped(response.bytes);
        drawSoon();
        break;
      case "logs_ended":
        end("ended", null);
        break;
      case "error":
        end("failed", response.message);
        break;
      default:
        // Nothing else belongs to a follow.
        break;
    }
  };

  draw();
  invoke("follow_logs", { ticket, name, cwd, onOutput: channel }).then(
    () => {
      // The command comes back when the Daemon ended the stream, or when Rust closed it because the window was hidden.
      if (live && (status === "connecting" || status === "following")) {
        end("paused", null);
      }
    },
    (reason: unknown) => {
      if (live) {
        end("failed", failureMessage(reason));
      }
    },
  );

  return () => {
    live = false;
    clearTimeout(timer);
    // Rust closes the connection, and the Daemon learns that nobody reads any more.
    invoke("stop_follow", { ticket: nextTicket() }).catch(() => undefined);
  };
}

/** Reads the output of `name` once. */
function snapshot(name: string, cwd: string, show: (shown: Shown) => void): () => void {
  let live = true;
  show(NOTHING);
  invoke<Response>("service_logs", { name, cwd }).then(
    (response) => {
      if (!live) {
        return;
      }
      const at = Date.now();
      if (response.response === "logs") {
        const transcript = new Transcript();
        transcript.write(response.output);
        show({ lines: transcript.lines(), status: "snapshot", message: null, at });
      } else if (response.response === "error") {
        show({ lines: [], status: "failed", message: response.message, at });
      } else {
        show({
          lines: [],
          status: "failed",
          message: `unexpected answer from the Daemon: ${response.response}`,
          at,
        });
      }
    },
    (reason: unknown) => {
      if (live) {
        show({ lines: [], status: "failed", message: failureMessage(reason), at: Date.now() });
      }
    },
  );
  return () => {
    live = false;
  };
}
