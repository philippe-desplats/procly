import type { FailureKey } from "./daemon";
import type { DoctorStore } from "./doctor";
import { WorktreeChecks } from "./DoctorScreen";
import { worktreeLabel, worktreeTooltip } from "./labels";
import { isSeen, useFailed } from "./needs-you";
import type { ForeignProcess } from "./protocol/ForeignProcess";
import type { JobStatus } from "./protocol/JobStatus";
import type { WorktreeStatus } from "./protocol/WorktreeStatus";
import { ContainerRow, JobRow, RemovedMark, SeenMark, ServiceRow } from "./Rows";
import { SelectableRow } from "./selectable";
import { jobWord } from "./state";
import { State } from "./StateMark";

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
 * One Worktree in detail: what runs in it that Procly did not start, then its Services, its Isolated Compose services
 * and its Jobs. It draws what the status returns and nothing else. A row picks the item for the inspector, which holds
 * the Stop and the Restart.
 */
export function WorktreeScreen({
  worktree,
  at,
  seen,
  doctor,
  onRelease,
}: {
  worktree: WorktreeStatus;
  at: number;
  seen: FailureKey[];
  doctor: DoctorStore;
  /**
   * Opens the release sheet. The app holds the sheet, not this screen: a release can leave the Daemon with nothing to
   * report for the Worktree, and the screen is gone by then while the sheet still shows the result.
   */
  onRelease: (worktree: WorktreeStatus) => void;
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
      <div className="actions-row">
        <button
          type="button"
          className="button"
          onClick={() => {
            onRelease(worktree);
          }}
        >
          Release…
        </button>
      </div>
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
              <ContainerRow key={container.service} container={container} worktreePath={worktree.path} />
            ))}
          </ul>
        </>
      ) : null}
      {worktree.jobs.length > 0 ? (
        <>
          <h3 className="caption">Jobs</h3>
          <ul className="list">
            {worktree.jobs.map((job) => (
              <JobRow key={job.id} job={job} seen={seen} />
            ))}
          </ul>
        </>
      ) : null}
      <WorktreeChecks store={doctor} path={worktree.path} />
    </section>
  );
}

/**
 * A Job of the screen outside any Repository, with the directory it started in. A row picks it for the inspector,
 * which follows its output as for any other Job.
 */
function OutsideJobRow({ job, seen }: { job: JobStatus; seen: FailureKey[] }) {
  const key: FailureKey = { kind: "job", id: job.id };
  const marked = isSeen(seen, key);
  const failed = useFailed(key);
  return (
    <SelectableRow target={{ kind: "job", id: job.id }} seen={marked}>
      <State {...jobWord(job, failed)} />
      <span className="item-name mono">{job.command}</span>
      <span className="caption mono">{job.id}</span>
      <span className="caption mono">{job.cwd}</span>
      <SeenMark seen={marked} />
    </SelectableRow>
  );
}

/**
 * The Jobs started in a directory outside any Repository. They have no Worktree, so nothing here stops them, and a row
 * picks the Job for the inspector.
 */
export function OutsideRepositoriesScreen({ jobs, seen }: { jobs: JobStatus[]; seen: FailureKey[] }) {
  return (
    <section aria-label="Outside Repositories">
      <h2 className="title">Outside Repositories</h2>
      {jobs.length === 0 ? <p className="muted">No Job started outside a Repository.</p> : null}
      <ul className="list">
        {jobs.map((job) => (
          <OutsideJobRow key={job.id} job={job} seen={seen} />
        ))}
      </ul>
    </section>
  );
}
