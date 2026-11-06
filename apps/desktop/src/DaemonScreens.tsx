import { useState } from "react";

import { CopyBox } from "./CopyBox";
import { failureMessage, type DaemonStore } from "./daemon";
import type { DoctorStore } from "./doctor";
import { ChecksWithoutDaemon } from "./DoctorScreen";

/** The command that has a session manager keep the Daemon running, which the developer runs in a terminal. */
const INSTALL_UNIT = "procly unit install";

/** The command the restart sheet stands for, run by hand. */
const STOP_DAEMON = "procly daemon stop";

/** What a click starts: the call, whether it is running, and why the last one failed. */
function useAction(run: () => Promise<void>): { start: () => void; pending: boolean; error: string | null } {
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const start = (): void => {
    setPending(true);
    setError(null);
    run()
      .catch((reason: unknown) => {
        setError(failureMessage(reason));
      })
      .finally(() => {
        setPending(false);
      });
  };
  return { start, pending, error };
}

/**
 * No Daemon answers. The app never starts one on its own: the developer does, with the button, and the window
 * recovers on the next answer of the poller. Beside the button, the checks of the installation that run without a
 * Daemon, which doctor ran when the app entered this state.
 */
export function DaemonNotRunning({ store, doctor }: { store: DaemonStore; doctor: DoctorStore }) {
  const start = useAction(() => store.startDaemon());
  return (
    <section>
      <h2 className="title">Daemon not running</h2>
      <p className="muted">No Daemon answers on its socket, and this app does not start one on its own.</p>
      <p>
        <button type="button" className="button brand" disabled={start.pending} onClick={start.start}>
          Start the Daemon
        </button>
      </p>
      {start.error === null ? null : (
        <p role="alert" className="error">
          {start.error}
        </p>
      )}
      <p className="caption">To have the Daemon start with every session, run in a terminal:</p>
      <CopyBox text={INSTALL_UNIT} />
      <ChecksWithoutDaemon store={doctor} />
    </section>
  );
}

/**
 * The Daemon speaks another protocol version than this app. An older one can be restarted behind a confirmation, a
 * newer one cannot: the app never replaces a Daemon by an older one.
 */
export function DaemonOfAnotherVersion({
  daemon,
  app,
  store,
}: {
  daemon: number;
  app: number;
  store: DaemonStore;
}) {
  const [confirming, setConfirming] = useState(false);
  return (
    <section>
      <h2 className="title">Daemon of another version</h2>
      <p>{`The Daemon speaks protocol ${daemon}, this app speaks ${app}.`}</p>
      {daemon < app ? (
        <>
          <p className="muted">
            A Daemon left by an earlier release still runs. Restart it to run the one this app embeds.
          </p>
          <p>
            <button
              type="button"
              className="button"
              onClick={() => {
                setConfirming(true);
              }}
            >
              Restart the Daemon
            </button>
          </p>
          {confirming ? (
            <RestartSheet
              store={store}
              onCancel={() => {
                setConfirming(false);
              }}
            />
          ) : null}
        </>
      ) : (
        <p className="muted">{`Update Procly to a release that speaks protocol ${daemon}. This app does not replace a newer Daemon.`}</p>
      )}
    </section>
  );
}

/** The sheet before a restart: what it does, what it costs, the command it stands for. Cancel is the default. */
function RestartSheet({ store, onCancel }: { store: DaemonStore; onCancel: () => void }) {
  const restart = useAction(() => store.restartDaemon());
  return (
    <div className="overlay">
      <div
        className="sheet"
        role="dialog"
        aria-modal="true"
        aria-labelledby="restart-title"
        onKeyDown={(event) => {
          if (event.key === "Escape" && !restart.pending) {
            onCancel();
          }
        }}
      >
        <h2 id="restart-title" className="title">
          Restart the Daemon
        </h2>
        <p>This stops the Daemon, then starts the one this app embeds.</p>
        <p>Every running Service will stop, in every Worktree.</p>
        <p className="caption">The same, from a terminal:</p>
        <CopyBox text={STOP_DAEMON} />
        {restart.error === null ? null : (
          <p role="alert" className="error">
            {restart.error}
          </p>
        )}
        {restart.pending ? <p className="caption">Restarting…</p> : null}
        <div className="actions">
          {/* Cancel takes the focus, so that Enter never restarts by reflex. */}
          <button
            type="button"
            className="button default"
            autoFocus
            disabled={restart.pending}
            onClick={onCancel}
          >
            Cancel
          </button>
          <button type="button" className="button" disabled={restart.pending} onClick={restart.start}>
            Restart the Daemon
          </button>
        </div>
      </div>
    </div>
  );
}
