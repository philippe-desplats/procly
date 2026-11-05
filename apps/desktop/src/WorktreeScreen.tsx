import { useState } from "react";

import { CopyBox } from "./CopyBox";
import { failureMessage, type FailureKey } from "./daemon";
import { worktreeLabel, worktreeTooltip } from "./labels";
import type { ContainerStatus } from "./protocol/ContainerStatus";
import type { ForeignProcess } from "./protocol/ForeignProcess";
import type { JobStatus } from "./protocol/JobStatus";
import type { WorktreeStatus } from "./protocol/WorktreeStatus";
import { JobRow, RemovedMark, ServiceRow } from "./Rows";
import { commandIn, restartService, stopService } from "./services";

/**
 * Stop and Restart of one Service, or a command to copy when the app cannot restart it. A failure stays under the
 * row, and every action ends in a refresh, since the Daemon may have changed whatever the answer says.
 */
function Controls({
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

function ComposeRow({
  container,
  cwd,
  onChanged,
}: {
  container: ContainerStatus;
  cwd: string;
  onChanged: () => void;
}) {
  return (
    <li className="item item-block">
      <div className="item-line">
        <span className="item-name">{container.service}</span>
        {container.port === null ? null : <span className="caption">{`:${container.port}`}</span>}
        <span className="caption">{`${container.project} / ${container.compose}`}</span>
      </div>
      {/* The status lists the containers that run, and does not tell whether the Daemon holds their environment. */}
      <Controls name={container.service} cwd={cwd} running restartable={false} onChanged={onChanged} />
    </li>
  );
}

function ForeignRow({ process }: { process: ForeignProcess }) {
  return (
    <li className="item">
      <span className="mark">Foreign</span>
      <span className="item-name mono">{process.kind === "process" ? process.command : process.name}</span>
      {process.kind === "process" ? (
        <span className="caption">{`pid ${process.pid}`}</span>
      ) : (
        <span className="caption">{`${process.project} / ${process.service}`}</span>
      )}
      {process.ports.length === 0 ? null : (
        <span className="caption">{process.ports.map((port) => `:${port}`).join(" ")}</span>
      )}
    </li>
  );
}

/**
 * One Worktree in detail: what runs in it that Procly did not start, then its Services and Isolated Compose services
 * with Stop and Restart, then its Jobs. It draws what the status returns and nothing else.
 */
export function WorktreeScreen({
  worktree,
  at,
  seen,
  onChanged,
}: {
  worktree: WorktreeStatus;
  at: number;
  seen: FailureKey[];
  onChanged: () => void;
}) {
  const foreign = worktree.foreign ?? [];
  const containers = worktree.containers ?? [];
  const empty =
    worktree.services.length === 0 &&
    worktree.jobs.length === 0 &&
    foreign.length === 0 &&
    containers.length === 0;
  return (
    <section>
      <h2 className="title">
        <span title={worktreeTooltip(worktree)}>{worktreeLabel(worktree)}</span>{" "}
        <RemovedMark worktree={worktree} />
      </h2>
      {empty ? <p className="muted">Nothing runs in this Worktree.</p> : null}
      {foreign.length > 0 ? (
        <>
          <h3 className="caption">Foreign processes</h3>
          <ul className="list">
            {foreign.map((process) => (
              <ForeignRow key={process.kind === "process" ? process.pid : process.name} process={process} />
            ))}
          </ul>
        </>
      ) : null}
      {worktree.services.length > 0 ? (
        <>
          <h3 className="caption">Services</h3>
          <ul className="list">
            {worktree.services.map((service) => (
              <ServiceRow
                key={service.service}
                service={service}
                worktreePath={worktree.path}
                at={at}
                seen={seen}
                controls={
                  <Controls
                    name={service.service}
                    cwd={worktree.path}
                    running={service.state !== "ended"}
                    restartable={service.restartable}
                    onChanged={onChanged}
                  />
                }
              />
            ))}
          </ul>
        </>
      ) : null}
      {containers.length > 0 ? (
        <>
          <h3 className="caption">Isolated Compose services</h3>
          <ul className="list">
            {containers.map((container) => (
              <ComposeRow
                key={container.service}
                container={container}
                cwd={worktree.path}
                onChanged={onChanged}
              />
            ))}
          </ul>
        </>
      ) : null}
      <JobList jobs={worktree.jobs} seen={seen} />
    </section>
  );
}

function JobList({ jobs, seen }: { jobs: JobStatus[]; seen: FailureKey[] }) {
  if (jobs.length === 0) {
    return null;
  }
  return (
    <>
      <h3 className="caption">Jobs</h3>
      <ul className="list">
        {jobs.map((job) => (
          <JobRow key={job.id} job={job} seen={seen} />
        ))}
      </ul>
    </>
  );
}

/** The Jobs started in a directory outside any Repository. They have no Worktree, so nothing here stops them. */
export function OutsideRepositoriesScreen({ jobs, seen }: { jobs: JobStatus[]; seen: FailureKey[] }) {
  return (
    <section aria-label="Outside Repositories">
      <h2 className="title">Outside Repositories</h2>
      {jobs.length === 0 ? <p className="muted">No Job started outside a Repository.</p> : null}
      <ul className="list">
        {jobs.map((job) => (
          <JobRow key={job.id} job={job} seen={seen} where={job.cwd} />
        ))}
      </ul>
    </section>
  );
}
