import { daemonVersion, problemCount, type DoctorDocument, type DoctorState } from "./doctor";
import { GlyphMark } from "./Glyph";
import { orphanCount, type OrphanScan } from "./orphan-scan";
import { formatMinute, formatMoment } from "./time";

/** What the sidebar reads of the last doctor run: its problems, the release it found the Daemon on, and when it ran. */
interface LastRun {
  problems: number;
  version: string | null;
  at: number;
}

function summarize(document: DoctorDocument, at: number): LastRun {
  return { problems: problemCount(document), version: daemonVersion(document), at };
}

/** The last doctor run the sidebar counts from: the one on show while a new one is on its way. */
function lastRun(state: DoctorState): LastRun | null {
  switch (state.kind) {
    case "report":
      return summarize(state.document, state.at);
    case "running":
      return state.document === null || state.at === null ? null : summarize(state.document, state.at);
    case "idle":
    case "failure":
      return null;
    default: {
      const unhandled: never = state;
      throw new Error(`unhandled state: ${JSON.stringify(unhandled)}`);
    }
  }
}

/**
 * The foot of the sidebar: Doctor with the problems of its last run and when it ran, Orphans with the count of the last
 * dry run, the line of the Daemon under them, and Settings pinned last. Every time is absolute, since an age would drift between two ticks.
 * A count is drawn only once a run happened and found something, so nothing is invented before the first run.
 * `updatedAt` is when the poller last read a Status, `null` while none was read.
 */
export function SidebarFooter({
  updatedAt,
  doctor,
  orphans,
  checking,
  orphansOpen,
  settings,
  onDoctor,
  onOrphans,
  onSettings,
}: {
  updatedAt: number | null;
  doctor: DoctorState;
  orphans: OrphanScan;
  checking: boolean;
  orphansOpen: boolean;
  settings: boolean;
  onDoctor: () => void;
  onOrphans: () => void;
  onSettings: () => void;
}) {
  const run = lastRun(doctor);
  const orphaned = orphanCount(orphans);
  const version = run?.version ?? null;
  return (
    <div className="sidebar-foot">
      <button type="button" className="row" aria-current={checking ? "true" : undefined} onClick={onDoctor}>
        <span className="row-name">Doctor</span>
        {run === null || run.problems === 0 ? null : (
          <span className="row-count">{`${problemsOf(run.problems)} at ${formatMinute(run.at, updatedAt ?? run.at)}`}</span>
        )}
      </button>
      <button
        type="button"
        className="row"
        aria-current={orphansOpen ? "true" : undefined}
        onClick={onOrphans}
      >
        <span className="row-name">Orphans</span>
        {orphaned === null || orphaned === 0 ? null : <span className="row-count">{orphaned}</span>}
      </button>
      {updatedAt === null ? null : (
        <p className="daemon-line">
          <GlyphMark glyph="healthy" />
          <span>{`Daemon${version === null ? "" : ` ${version}`}, updated at ${formatMoment(updatedAt, updatedAt)}`}</span>
        </p>
      )}
      <button type="button" className="row" aria-current={settings ? "true" : undefined} onClick={onSettings}>
        <span className="row-name">Settings</span>
        <span className="caption">⌘,</span>
      </button>
    </div>
  );
}

function problemsOf(count: number): string {
  return count === 1 ? "1 problem" : `${count} problems`;
}
