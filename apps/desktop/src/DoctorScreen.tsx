import { Checks, type ShowAction } from "./CheckList";
import { CopyBox } from "./CopyBox";
import {
  checksWithoutDaemon,
  machineChecks,
  releaseDrift,
  useDoctor,
  useDoctorOpen,
  worktreeChecks,
  type DoctorDocument,
  type DoctorState,
  type DoctorStore,
} from "./doctor";
import type { Check } from "./protocol/Check";
import { formatMoment } from "./time";

/** How many checks failed and how many only warn, in a line. */
function summary(document: DoctorDocument): string {
  const count = (status: string): number => document.checks.filter((check) => check.status === status).length;
  const failed = count("failed");
  const warnings = count("warning");
  if (failed === 0 && warnings === 0) {
    return "Every check passed.";
  }
  const problems = failed === 1 ? "1 check failed" : `${failed} checks failed`;
  return warnings === 0 ? problems : `${problems}, ${warnings === 1 ? "1 warning" : `${warnings} warnings`}`;
}

/**
 * The checks of the machine and the Daemon, problems first, each fix behind a Copy button, and the prompt to hand to
 * the developer's own agent. Doctor runs when the screen opens, and again on Refresh.
 */
export function DoctorScreen({
  store,
  onShowOrphans,
  onShowProcess,
}: {
  store: DoctorStore;
  onShowOrphans: () => void;
  /** Takes to the Foreign process that holds a port a Lease holds, `null` while the Status reports none. */
  onShowProcess: (() => void) | null;
}) {
  useDoctorOpen(store, null);
  const state = useDoctor(store, null);
  const document = state.kind === "report" || state.kind === "running" ? state.document : null;
  // Where to look for a problem is the screen's to say: the Orphans screen lists the Orphans, the Worktree names the
  // Foreign process. A check that passed has no problem to look for.
  const show = (check: Check): ShowAction | null => {
    if (check.status !== "failed") {
      return null;
    }
    if (check.name === "orphans") {
      return { label: "Show Orphans", onShow: onShowOrphans };
    }
    return check.name === "leased_ports" && onShowProcess !== null
      ? { label: "Show the Process", onShow: onShowProcess }
      : null;
  };
  return (
    <section aria-label="Doctor">
      <h2 className="title">Doctor</h2>
      <p className="muted">
        The checks of this Mac and of the Daemon. Doctor changes nothing and runs none of the commands it
        names.
      </p>
      {document === null ? null : <p className="caption">{summary(document)}</p>}
      {state.kind === "report" ? (
        <p className="caption">{`Checked ${formatMoment(state.at, state.at)}`}</p>
      ) : null}
      <Checks state={state} pick={machineChecks} fold show={show} />
      <p className="doctor-foot">
        The checks of a Worktree (configuration, Guard, Worktrunk hooks) are in its view.
      </p>
      {document === null || document.prompt === null ? null : <AgentPrompt prompt={document.prompt} />}
    </section>
  );
}

/** What doctor would hand to a coding agent for the failed checks, to paste into the developer's own. */
function AgentPrompt({ prompt }: { prompt: string }) {
  return (
    <div role="group" aria-label="Prompt for your agent" className="prompt">
      <h3 className="subtitle">Hand it to your agent</h3>
      <p className="caption">Paste this prompt into your own coding agent to have it fix the problems.</p>
      <CopyBox text={prompt} block />
    </div>
  );
}

/** The doctor checks of one Worktree: doctor runs from its directory when the Worktree screen opens. */
export function WorktreeChecks({ store, path }: { store: DoctorStore; path: string }) {
  useDoctorOpen(store, path);
  const state = useDoctor(store, path);
  return (
    <section aria-label="Doctor checks" className="worktree-checks">
      <h3 className="caption">Doctor checks</h3>
      <Checks state={state} pick={worktreeChecks} worktree={path} fold />
    </section>
  );
}

/**
 * The checks that run without the Daemon, for the screen that tells it does not run. The run itself starts when the
 * app enters that state (see `App`), so this only reads what it found.
 */
export function ChecksWithoutDaemon({ store }: { store: DoctorStore }) {
  const state = useDoctor(store, null);
  // The report kept from before the Daemon stopped says it answers, so nothing is shown until doctor ran again.
  const fresh: DoctorState = state.kind === "running" ? { kind: "running", document: null, at: null } : state;
  return (
    <section aria-label="Checks without the Daemon">
      <h3 className="subtitle">Checks that run without the Daemon</h3>
      <Checks state={fresh} pick={checksWithoutDaemon} />
    </section>
  );
}

/**
 * The Daemon answers in the protocol of this app but runs another release of Procly. Doctor says so, and the banner
 * only tells: nothing is blocked, restarting is the developer's call.
 */
export function ReleaseBanner({ store, onOpenDoctor }: { store: DoctorStore; onOpenDoctor: () => void }) {
  const state = useDoctor(store, null);
  // A run that refreshes the report keeps the banner up until it answers.
  const document = state.kind === "report" || state.kind === "running" ? state.document : null;
  const drift = document === null ? null : releaseDrift(document);
  if (drift === null) {
    return null;
  }
  return (
    <div role="status" className="banner">
      <span>{drift.message}</span>
      <button type="button" className="button" onClick={onOpenDoctor}>
        Open Doctor
      </button>
    </div>
  );
}
