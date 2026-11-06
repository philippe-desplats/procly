import { useState } from "react";

import { CopyBox } from "./CopyBox";
import { failureMessage } from "./daemon";
import { useTauriEvent } from "./doctor";
import { MENU_CHOICE, type MenuChoice } from "./menu";
import { commandIn, restartService, stopService } from "./services";

/**
 * Stop and Restart of one Service, or a command to copy when the app cannot restart it. A failure stays under the
 * buttons, and every action ends in a refresh, since the Daemon may have changed whatever the answer says.
 */
export function ServiceActions({
  name,
  cwd,
  running,
  restartable,
  onChanged,
}: {
  name: string;
  cwd: string;
  /** Whether there is something to stop. */
  running: boolean;
  /** Whether the Daemon holds the environment a Restart replays, `false` when the status does not tell. */
  restartable: boolean;
  onChanged: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  function run(action: Promise<void>): void {
    setBusy(true);
    setError(null);
    action
      .catch((reason: unknown) => {
        setError(failureMessage(reason));
      })
      .finally(() => {
        setBusy(false);
        onChanged();
      });
  }

  // Restart and Stop of the menu bar act on the selection, which is this Service, as these buttons do.
  useTauriEvent<MenuChoice>(MENU_CHOICE, (choice) => {
    if (busy) {
      return;
    }
    if (choice === "restart" && restartable) {
      run(restartService(name, cwd));
    } else if (choice === "stop" && running) {
      run(stopService(name, cwd));
    }
  });

  return (
    <div className="controls">
      <div className="controls-buttons">
        {running ? (
          <button
            type="button"
            className="button"
            aria-label={`Stop ${name}`}
            disabled={busy}
            onClick={() => {
              run(stopService(name, cwd));
            }}
          >
            Stop
          </button>
        ) : null}
        {restartable ? (
          <button
            type="button"
            className="button"
            aria-label={`Restart ${name}`}
            disabled={busy}
            onClick={() => {
              run(restartService(name, cwd));
            }}
          >
            Restart
          </button>
        ) : null}
      </div>
      {restartable ? null : <CopyBox text={commandIn(cwd, `restart ${name}`)} />}
      {error === null ? null : (
        <p role="alert" className="error">
          {error}
        </p>
      )}
    </div>
  );
}
