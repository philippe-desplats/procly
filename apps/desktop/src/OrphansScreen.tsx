import { useEffect, useState } from "react";

import { CopyBox } from "./CopyBox";
import { failureMessage } from "./daemon";
import {
  collectionEffects,
  COLLECT_COMMAND,
  deleteOrphans,
  describeOrphan,
  groupByWorktree,
  removedWorktreeLabel,
} from "./orphans";
import { reportOfScan, useOrphanScan, type OrphanScan, type OrphanScanStore } from "./orphan-scan";
import type { GcReport } from "./protocol/GcReport";
import type { Orphan } from "./protocol/Orphan";

/**
 * The Orphans the Daemon found, grouped by the removed Worktree that left them, each naming the data its deletion
 * drops. The screen shows the `orphans` of the last dry run, which it asks for when it opens, and the sheet deletes
 * those and nothing else.
 */
export function OrphansScreen({ store }: { store: OrphanScanStore }) {
  const scan = useOrphanScan(store);
  const [sheet, setSheet] = useState<"closed" | "confirm">("closed");

  useEffect(() => {
    void store.run();
  }, [store]);

  const report = reportOfScan(scan);

  return (
    <section aria-label="Orphans">
      <h2 className="title">Orphans</h2>
      <p className="muted">
        What removed Worktrees left behind: Leases, Logical resources, output and setup directories, Isolated
        containers and their volumes.
      </p>
      <ScanView
        scan={scan}
        onLook={() => {
          void store.run();
        }}
        onDelete={() => {
          setSheet("confirm");
        }}
      />
      {sheet === "confirm" && report !== null ? (
        <CollectSheet
          shown={report.orphans}
          onClose={() => {
            setSheet("closed");
            void store.run();
          }}
        />
      ) : null}
    </section>
  );
}

function ScanView({
  scan,
  onLook,
  onDelete,
}: {
  scan: OrphanScan;
  onLook: () => void;
  onDelete: () => void;
}) {
  if (scan.kind === "failure") {
    return (
      <>
        <p role="alert" className="error">
          {scan.message}
        </p>
        <button type="button" className="button" onClick={onLook}>
          Look again
        </button>
      </>
    );
  }
  const report = reportOfScan(scan);
  if (report === null) {
    return <p className="caption">Looking for Orphans…</p>;
  }
  const { orphans, skipped, warnings } = report;
  return (
    <>
      {orphans.length === 0 ? (
        <p>No Orphan.</p>
      ) : (
        <div className="actions-row">
          <button type="button" className="button" onClick={onDelete}>
            {`Delete ${orphansCount(orphans.length)}…`}
          </button>
        </div>
      )}
      {groupByWorktree(orphans).map((group) => {
        const label = removedWorktreeLabel(group.worktree);
        return (
          <section key={`${group.repository}\n${group.worktree}`} aria-label={label} className="group">
            <h3 className="subtitle">{label}</h3>
            <p className="caption mono">{group.worktree}</p>
            <ul className="list">
              {group.orphans.map((orphan, index) => {
                const line = describeOrphan(orphan);
                return (
                  <li key={index} className="item item-block">
                    <span className="item-name">{line.what}</span>
                    <span className={line.loss ? "loss" : "muted"}>{line.drops}</span>
                  </li>
                );
              })}
            </ul>
          </section>
        );
      })}
      {skipped.map((repository) => (
        <p key={repository.repository} className="caption">
          {`${repository.repository}: cannot list its Worktrees, nothing of it is an Orphan (${repository.error})`}
        </p>
      ))}
      {warnings.map((warning) => (
        <p key={warning} className="caption">
          {warning}
        </p>
      ))}
    </>
  );
}

function orphansCount(count: number): string {
  return count === 1 ? "1 Orphan" : `${count} Orphans`;
}

/** The sheet before a collection, then its result: what it deletes, what it costs, the command it stands for. */
function CollectSheet({ shown, onClose }: { shown: Orphan[]; onClose: () => void }) {
  const [state, setState] = useState<
    { kind: "confirm"; error: string | null } | { kind: "deleting" } | { kind: "result"; report: GcReport }
  >({ kind: "confirm", error: null });
  const label = `Delete ${orphansCount(shown.length)}`;

  function collect(): void {
    setState({ kind: "deleting" });
    // The Orphans are sent back as the dry run returned them, never a bare apply.
    deleteOrphans(shown).then(
      (report) => {
        setState({ kind: "result", report });
      },
      (reason: unknown) => {
        setState({ kind: "confirm", error: failureMessage(reason) });
      },
    );
  }

  if (state.kind === "result") {
    return <ResultSheet report={state.report} onDone={onClose} />;
  }
  const pending = state.kind === "deleting";
  const effects = collectionEffects(shown);
  return (
    <div className="overlay">
      <div
        className="sheet"
        role="dialog"
        aria-modal="true"
        aria-labelledby="collect-title"
        onKeyDown={(event) => {
          if (event.key === "Escape" && !pending) {
            onClose();
          }
        }}
      >
        <h2 id="collect-title" className="title">
          {label}
        </h2>
        <p>Each one is deleted from the Daemon and from disk:</p>
        <ul aria-label="What it deletes" className="list">
          {effects.map((effect) => (
            <li key={effect.text} className={effect.loss ? "item loss" : "item"}>
              {effect.text}
            </li>
          ))}
        </ul>
        <p className={effects.some((effect) => effect.loss) ? "loss" : undefined}>This cannot be undone.</p>
        <p className="caption">From a terminal, which deletes every Orphan it finds, not only these:</p>
        <CopyBox text={COLLECT_COMMAND} />
        {state.kind === "confirm" && state.error !== null ? (
          <p role="alert" className="error">
            {state.error}
          </p>
        ) : null}
        {pending ? <p className="caption">Deleting…</p> : null}
        <div className="actions">
          {/* Cancel takes the focus, so that Enter never deletes by reflex. */}
          <button type="button" className="button default" autoFocus disabled={pending} onClick={onClose}>
            Cancel
          </button>
          <button type="button" className="button" disabled={pending} onClick={collect}>
            {label}
          </button>
        </div>
      </div>
    </div>
  );
}

/** What the collection did, item by item: the Orphans not deleted come with their reason. */
function ResultSheet({ report, onDone }: { report: GcReport; onDone: () => void }) {
  return (
    <div className="overlay">
      <div className="sheet" role="dialog" aria-modal="true" aria-labelledby="result-title">
        <h2 id="result-title" className="title">
          Result
        </h2>
        <ul className="list">
          {report.orphans.map((orphan, index) => (
            <li key={index} className="item item-block">
              <span className="item-name">{describeOrphan(orphan).what}</span>
              <span className="caption">{removedWorktreeLabel(orphan.worktree)}</span>
              {orphan.deleted ? (
                <span className="muted">Deleted</span>
              ) : (
                <span className="error">{`Not deleted: ${orphan.error ?? "the Daemon gave no reason"}`}</span>
              )}
            </li>
          ))}
        </ul>
        {report.warnings.map((warning) => (
          <p key={warning} className="caption">
            {warning}
          </p>
        ))}
        <div className="actions">
          <button type="button" className="button default" autoFocus onClick={onDone}>
            Done
          </button>
        </div>
      </div>
    </div>
  );
}
