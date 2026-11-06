import { useEffect, useState } from "react";

import { CopyBox } from "./CopyBox";
import { failureMessage } from "./daemon";
import {
  composeLine,
  endedWords,
  holdsNothing,
  previewRelease,
  releaseCommand,
  releaseWorktree,
  type Ended,
} from "./release";
import type { KeptLease } from "./protocol/KeptLease";
import type { ReleaseReport } from "./protocol/ReleaseReport";

/** Whether a report says what a release would do, or what it did. Only the verbs of the headings differ. */
type Tense = "preview" | "result";

type Phase =
  | { kind: "reading" }
  | { kind: "failure"; message: string }
  | { kind: "confirm"; report: ReleaseReport; error: string | null }
  | { kind: "releasing"; report: ReleaseReport }
  | { kind: "result"; report: ReleaseReport };

function keptLeaseLine(kept: KeptLease): string {
  const lease = `Lease ${kept.service}.${kept.port_name}, port ${kept.port}`;
  return kept.reason === "pinned_branch"
    ? `${lease}, kept for Pinned branch ${kept.branch}`
    : `${lease}, kept because its container still runs`;
}

/** The effects of a release, one list per kind, each drawn only when it holds something. */
function Effects({ report, tense }: { report: ReleaseReport; tense: Tense }) {
  const preview = tense === "preview";
  return (
    <>
      {report.services.length + report.jobs.length > 0 ? (
        <>
          <h3 className="caption">{preview ? "Will stop" : "Stopped"}</h3>
          <ul className="list">
            {report.services.map((service) => (
              <li key={`service:${service}`} className="item">
                <span className="item-name">{service}</span>
              </li>
            ))}
            {report.jobs.map((job) => (
              <li key={`job:${job}`} className="item">
                <span className="item-name">{`Job ${job}`}</span>
              </li>
            ))}
          </ul>
        </>
      ) : null}
      {report.compose_services.length > 0 ? (
        <>
          <h3 className="caption">{preview ? "Will let go of" : "Let go of"}</h3>
          <ul className="list">
            {report.compose_services.map((released) => (
              <li key={released.service} className="item">
                <span className="item-name">{composeLine(released)}</span>
              </li>
            ))}
          </ul>
        </>
      ) : null}
      {report.leases.length > 0 ? (
        <>
          <h3 className="caption">{preview ? "Will release" : "Released"}</h3>
          <ul className="list">
            {report.leases.map((lease) => (
              <li key={`${lease.service}.${lease.port_name}`} className="item">
                <span className="item-name">{`Lease ${lease.service}.${lease.port_name}, port ${lease.port}`}</span>
              </li>
            ))}
          </ul>
        </>
      ) : null}
      {report.kept_leases.length + report.logical_resources.length > 0 ? (
        <>
          <h3 className="caption">Kept</h3>
          <ul className="list">
            {report.kept_leases.map((kept) => (
              <li key={`${kept.service}.${kept.port_name}`} className="item item-block">
                <span className="item-name">{keptLeaseLine(kept)}</span>
              </li>
            ))}
            {report.logical_resources.map((resource) => (
              <li key={`${resource.service}.${resource.resource}`} className="item item-block">
                <span className="item-name">
                  {`Logical resource ${resource.name} of ${resource.service} (${resource.driver})`}
                </span>
                <span className="muted">Not deleted, procly gc lists it once the Worktree is removed.</span>
              </li>
            ))}
          </ul>
        </>
      ) : null}
      {report.warnings.map((warning) => (
        <p key={warning} className="caption">
          {warning}
        </p>
      ))}
    </>
  );
}

/**
 * The sheet that releases a Worktree. It reads what a release would do once, when it opens, with a dry run, then
 * names every effect and the command it stands for. Confirming sends the real release and shows the report it
 * returns, which is not compared with the dry run. Cancel takes the focus, so that Enter never releases by reflex.
 */
export function ReleaseSheet({
  path,
  label,
  listed,
  ended,
  onClose,
  onChanged,
}: {
  path: string;
  label: string;
  /** Whether git still lists the Worktree, in which case an agent may still be working in it. */
  listed: boolean;
  /** The ended Jobs and Services the status held for the Worktree when the sheet opened, which a release forgets. */
  ended: Ended;
  onClose: () => void;
  /** Called once the real release is over, so the window shows its effect without waiting for the poller. */
  onChanged: () => void;
}) {
  const [phase, setPhase] = useState<Phase>({ kind: "reading" });

  useEffect(() => {
    let current = true;
    previewRelease(path).then(
      (report) => {
        if (current) {
          setPhase({ kind: "confirm", report, error: null });
        }
      },
      (reason: unknown) => {
        if (current) {
          setPhase({ kind: "failure", message: failureMessage(reason) });
        }
      },
    );
    return () => {
      current = false;
    };
  }, [path]);

  function release(shown: ReleaseReport): void {
    setPhase({ kind: "releasing", report: shown });
    releaseWorktree(path).then(
      (report) => {
        setPhase({ kind: "result", report });
        onChanged();
      },
      (reason: unknown) => {
        setPhase({ kind: "confirm", report: shown, error: failureMessage(reason) });
        onChanged();
      },
    );
  }

  if (phase.kind === "result") {
    return (
      <ResultSheet label={label} report={phase.report} forgot={endedWords(ended) !== ""} onDone={onClose} />
    );
  }
  const pending = phase.kind === "releasing";
  const report = phase.kind === "confirm" || phase.kind === "releasing" ? phase.report : null;
  // Nothing runs, and nothing ended is left for the release to forget: the one case with no point in confirming.
  const forgotten = endedWords(ended);
  const running = report !== null && !holdsNothing(report);
  const nothing = report !== null && !running && forgotten === "";
  return (
    <div className="overlay">
      <div
        className="sheet"
        role="dialog"
        aria-modal="true"
        aria-labelledby="release-title"
        onKeyDown={(event) => {
          if (event.key === "Escape" && !pending) {
            onClose();
          }
        }}
      >
        <h2 id="release-title" className="title">
          {`Release ${label}`}
        </h2>
        {listed ? (
          <p role="alert" className="warning">
            Git still lists this Worktree, so an agent may still be working in it. A release stops what it
            runs.
          </p>
        ) : null}
        {phase.kind === "reading" ? <p className="caption">Reading what a release would do…</p> : null}
        {phase.kind === "failure" ? (
          <p role="alert" className="error">
            {phase.message}
          </p>
        ) : null}
        {report === null ? null : (
          <>
            {nothing ? (
              <p>Procly holds nothing in this Worktree, so there is nothing to release.</p>
            ) : running ? (
              <p>
                Each Service and Job is stopped with its whole process tree, and what it held in memory is
                lost.
              </p>
            ) : (
              <p>Nothing runs in this Worktree.</p>
            )}
            <Effects report={report} tense="preview" />
            {forgotten === "" ? null : (
              <p>
                {`The release forgets ${forgotten} of this Worktree, so procly wait, procly logs and procly status no longer know them.`}
              </p>
            )}
            {nothing ? null : <p>No data is deleted.</p>}
            <p className="caption">From a terminal, in the Worktree:</p>
            <CopyBox text={releaseCommand(path)} />
          </>
        )}
        {phase.kind === "confirm" && phase.error !== null ? (
          <p role="alert" className="error">
            {phase.error}
          </p>
        ) : null}
        {pending ? <p className="caption">Releasing…</p> : null}
        <div className="actions">
          <button type="button" className="button default" autoFocus disabled={pending} onClick={onClose}>
            Cancel
          </button>
          <button
            type="button"
            className="button"
            disabled={report === null || nothing || pending}
            onClick={() => {
              if (report !== null) {
                release(report);
              }
            }}
          >
            Release
          </button>
        </div>
      </div>
    </div>
  );
}

/** What the release did, as the Daemon reported it. */
function ResultSheet({
  label,
  report,
  forgot,
  onDone,
}: {
  label: string;
  report: ReleaseReport;
  /** Whether the sheet had ended Jobs or Services to forget, which the report does not list. */
  forgot: boolean;
  onDone: () => void;
}) {
  return (
    <div className="overlay">
      <div className="sheet" role="dialog" aria-modal="true" aria-labelledby="released-title">
        <h2 id="released-title" className="title">
          {`Released ${label}`}
        </h2>
        {holdsNothing(report) ? (
          <p>
            {forgot
              ? "Nothing ran in this Worktree."
              : "Procly held nothing in this Worktree, nothing was released."}
          </p>
        ) : null}
        {forgot ? <p>Its ended Jobs and Services were forgotten.</p> : null}
        <Effects report={report} tense="result" />
        <div className="actions">
          <button type="button" className="button default" autoFocus onClick={onDone}>
            Done
          </button>
        </div>
      </div>
    </div>
  );
}
