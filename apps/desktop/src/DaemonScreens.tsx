import { useState, type ReactNode } from "react";

import { BrandMark } from "./BrandMark";
import { CopyBox } from "./CopyBox";
import { failureMessage, type DaemonSnapshot, type DaemonStore } from "./daemon";
import { useDoctor, type DoctorStore } from "./doctor";
import { ChecksWithoutDaemon } from "./DoctorScreen";
import { DaemonLine, daemonVersionOf } from "./SidebarFooter";

/** The command that has a session manager keep the Daemon running, which the developer runs in a terminal. */
const INSTALL_UNIT = "procly unit install";

/** The command the restart sheet stands for, run by hand. */
const STOP_DAEMON = "procly daemon stop";

/** The command that updates the app to a release matching a newer Daemon, which the developer runs in a terminal. */
const UPDATE_APP = "brew upgrade --cask procly";

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

/** The snapshots with nothing to show in a sidebar or an inspector, which take the whole window instead. */
export type BlockingSnapshot = Extract<
  DaemonSnapshot,
  { kind: "no-daemon" | "protocol-mismatch" | "failure" }
>;

/**
 * Whether the Daemon cannot be read, gave an error or an unexpected answer, or speaks another protocol, so that the
 * window shows a state screen.
 */
export function isBlocking(snapshot: DaemonSnapshot): snapshot is BlockingSnapshot {
  return (
    snapshot.kind === "no-daemon" || snapshot.kind === "protocol-mismatch" || snapshot.kind === "failure"
  );
}

/**
 * The screen of a Daemon that cannot be used, on the whole window and not in the 3-pane shell. `lastAnswer` is when a
 * Status last answered, `null` while none did.
 */
export function DaemonStateScreen({
  snapshot,
  store,
  doctor,
  lastAnswer,
}: {
  snapshot: BlockingSnapshot;
  store: DaemonStore;
  doctor: DoctorStore;
  lastAnswer: number | null;
}) {
  switch (snapshot.kind) {
    case "no-daemon":
      return <DaemonNotRunning store={store} doctor={doctor} />;
    case "protocol-mismatch":
      return <DaemonOfAnotherVersion daemon={snapshot.daemon} app={snapshot.app} store={store} />;
    case "failure":
      return <DaemonDidNotAnswer message={snapshot.message} doctor={doctor} lastAnswer={lastAnswer} />;
    default: {
      const unhandled: never = snapshot;
      throw new Error(`unhandled snapshot: ${JSON.stringify(unhandled)}`);
    }
  }
}

/**
 * A narrow column at the top: the brand mark, the title, then what the developer can do. `below` is a block wider
 * than the column. The overlay title bar leaves the top of the window to the traffic lights, so a strip there moves
 * the window, as the one of the sidebar does.
 */
function StateScreen({
  title,
  children,
  below = null,
}: {
  title: string;
  children: ReactNode;
  below?: ReactNode;
}) {
  return (
    <>
      <div className="state-drag" data-tauri-drag-region />
      <main className="state-screen">
        <div className="state-column">
          <BrandMark />
          <h2 className="title">{title}</h2>
          {children}
        </div>
        {below}
      </main>
    </>
  );
}

/**
 * The Daemon gave an error or an answer this app cannot read. The poller keeps asking, and the window recovers on the
 * next Status. The line of the Daemon keeps the time of the last Status that answered, without the glyph that says it
 * answers.
 */
function DaemonDidNotAnswer({
  message,
  doctor,
  lastAnswer,
}: {
  message: string;
  doctor: DoctorStore;
  lastAnswer: number | null;
}) {
  const version = daemonVersionOf(useDoctor(doctor, null));
  return (
    <StateScreen title="The Daemon did not answer">
      <p className="muted">{message}</p>
      {lastAnswer === null ? null : <DaemonLine version={version} updatedAt={lastAnswer} answering={false} />}
    </StateScreen>
  );
}

/**
 * No Daemon answers. The app never starts one on its own: the developer does, with the button, and the window
 * recovers on the next answer of the poller. Beside the button, the checks of the installation that run without a
 * Daemon, which doctor ran when the app entered this state.
 */
function DaemonNotRunning({ store, doctor }: { store: DaemonStore; doctor: DoctorStore }) {
  const start = useAction(() => store.startDaemon());
  return (
    <StateScreen title="The Daemon is not running" below={<ChecksWithoutDaemon store={doctor} />}>
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
    </StateScreen>
  );
}

/**
 * The Daemon speaks another protocol version than this app. An older one can be restarted behind a confirmation, a
 * newer one cannot: the app never replaces a Daemon by an older one.
 */
function DaemonOfAnotherVersion({ daemon, app, store }: { daemon: number; app: number; store: DaemonStore }) {
  const [confirming, setConfirming] = useState(false);
  return (
    <StateScreen
      title={daemon < app ? "The Daemon runs an older version" : "The Daemon runs a newer version"}
    >
      <p>{`The Daemon speaks protocol ${daemon}, this app speaks ${app}.`}</p>
      {daemon < app ? (
        <>
          <p className="muted">
            A Daemon left by an earlier release still runs. Restart it to run the one this app embeds.
          </p>
          <div className="state-warning">
            <p>
              <b>Restarting it stops every Service and Job of every Repository.</b>
            </p>
            <p className="muted">They come back once your agents or you run procly up again.</p>
          </div>
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
          <p className="caption">Or stop it yourself from a terminal:</p>
          <CopyBox text={STOP_DAEMON} />
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
        <>
          <p className="muted">{`Update Procly to a release that speaks protocol ${daemon}. This app does not replace a newer Daemon.`}</p>
          <p className="caption">Update the app to match:</p>
          <CopyBox text={UPDATE_APP} />
        </>
      )}
    </StateScreen>
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
        <p className="loss">Every running Service will stop, in every Worktree.</p>
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
