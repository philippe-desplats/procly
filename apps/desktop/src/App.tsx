import { listen } from "@tauri-apps/api/event";
import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
  type ReactNode,
} from "react";

import { BrandMark } from "./BrandMark";
import { CopyBox } from "./CopyBox";
import { DaemonStateScreen, isBlocking, type BlockingSnapshot } from "./DaemonScreens";
import type { DaemonSnapshot, DaemonStore, FailureKey, NeedsYou } from "./daemon";
import {
  createDoctorStore,
  REFRESH_REQUESTED,
  useDoctor,
  useTauriEvent,
  type DoctorState,
  type DoctorStore,
} from "./doctor";
import { DoctorScreen, ReleaseBanner } from "./DoctorScreen";
import { GlyphMark } from "./Glyph";
import { ItemRow } from "./ItemRow";
import { basename, repositoryLabel, worktreeLabel, worktreeTooltip } from "./labels";
import { MENU_CHOICE, menuContext, useMenuContext } from "./menu";
import { failureKey, FailuresContext, failuresOf, reasonOf, type Reason } from "./needs-you";
import { createOrphanScanStore, useOrphanScan, type OrphanScan, type OrphanScanStore } from "./orphan-scan";
import { OrphansScreen } from "./OrphansScreen";
import { endedIn, type Ended } from "./release";
import { ReleaseSheet } from "./ReleaseSheet";
import { SelectionContext } from "./selectable";
import { foreignId, foreignToShow, resolve, selectedKey, type Selected, type Selection } from "./selection";
import { failureKeyOf, SelectedPanel, WorktreeNames } from "./SelectedPanel";
import { SettingsScreen } from "./SettingsScreen";
import { SidebarFooter } from "./SidebarFooter";
import { OPEN_FAILURE, OPEN_SETTINGS, openWorktree, revealWorktree, type Place } from "./settings";
import type { ForeignProcess } from "./protocol/ForeignProcess";
import type { MenuChoice } from "./protocol/MenuChoice";
import type { RepositoryStatus } from "./protocol/RepositoryStatus";
import type { ServiceStatus } from "./protocol/ServiceStatus";
import type { WorktreeStatus } from "./protocol/WorktreeStatus";
import { ContainerRow, RemovedMark, ServiceRow, worktreeWhere } from "./Rows";
import { foreignWord, jobEndWord, serviceEndWord } from "./state";
import { TitleBar } from "./TitleBar";
import { formatMoment } from "./time";
import { useOpening } from "./useOpening";
import { OutsideRepositoriesScreen, WorktreeScreen } from "./WorktreeScreen";

/**
 * What the center shows: the Overview of every Service, one Worktree, the Jobs outside any Repository, the Orphans,
 * Doctor, or Settings. The Orphans and Settings hide the inspector. `focusEditor`
 * sends the developer to the editor field, because "Open in Editor" found none set.
 */
type View =
  | { kind: "overview" }
  | { kind: "worktree"; path: string }
  | { kind: "outside" }
  | { kind: "orphans" }
  | { kind: "doctor" }
  | { kind: "settings"; focusEditor: boolean };

/**
 * `doctor` is the store of the checks and `orphans` the one of the last dry run of `Gc`, a window makes its own unless a
 * test hands it one.
 */
export function App({
  store,
  doctor: given,
  orphans: givenOrphans,
}: {
  store: DaemonStore;
  doctor?: DoctorStore;
  orphans?: OrphanScanStore;
}) {
  const snapshot = useSyncExternalStore(store.subscribe, store.getSnapshot);
  const [view, setCurrentView] = useState<View>({ kind: "overview" });
  const [selection, setSelection] = useState<Selection | null>(null);
  // Another screen shows another set of items, so what was picked in the one before is no longer on show.
  const setView = useCallback((next: View): void => {
    setCurrentView(next);
    setSelection(null);
  }, []);
  const control = useMemo(() => ({ selection, select: setSelection }), [selection]);
  // Which ends are failures is Rust's to say, the rows and the inspector draw them from this.
  const failures = useMemo(
    () => (snapshot.kind === "status" ? failuresOf(snapshot.needsYou, snapshot.seen) : []),
    [snapshot],
  );
  const [doctor] = useState(() => given ?? createDoctorStore());
  const [orphanScan] = useState(() => givenOrphans ?? createOrphanScanStore());
  // The Worktree whose release sheet is open. The app holds it, since the status may stop reporting the Worktree once
  // it is released, and the screen that opened the sheet goes with it.
  const [releasing, setReleasing] = useState<{
    path: string;
    label: string;
    listed: boolean;
    ended: Ended;
    foreign: ForeignProcess[];
  } | null>(null);

  // The poller of the app keeps the snapshot current, the window only follows it.
  useEffect(() => store.start(), [store]);

  // When a Status last answered, which the screen of a Daemon that no longer answers well still names.
  const [lastAnswer, setLastAnswer] = useState<number | null>(null);
  // Kept while rendering, as React advises for state that follows a prop, so a failure never shows a stale time.
  if (snapshot.kind === "status" && snapshot.at !== lastAnswer) {
    setLastAnswer(snapshot.at);
  }

  // Doctor runs when the app enters "Daemon not running", not on each tick that finds it still not running.
  const notRunning = snapshot.kind === "no-daemon";
  useEffect(() => {
    if (notRunning) {
      void doctor.run(null);
    }
  }, [doctor, notRunning]);

  // The release banner reads the machine checks, and the sidebar counts the Orphans, so both run once when the Daemon
  // first answers, on no timer, rather than waiting for a visit to Doctor or the Orphans or a Refresh.
  const answering = snapshot.kind === "status";
  const launched = useRef(false);
  useEffect(() => {
    if (answering && !launched.current) {
      launched.current = true;
      void doctor.run(null);
      void orphanScan.run();
    }
  }, [doctor, orphanScan, answering]);

  // The menu refreshes without the window, so Rust says so. A scan of the Orphans asks the Daemon, so it waits for one.
  useTauriEvent(REFRESH_REQUESTED, () => {
    void doctor.refresh();
    if (answering) {
      void orphanScan.run();
    }
  });

  // The native menu sends Settings, which `⌘,` triggers.
  useEffect(() => {
    let stopped = false;
    let unlisten: (() => void) | null = null;
    void listen(OPEN_SETTINGS, () => {
      setView({ kind: "settings", focusEditor: false });
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
  }, [setView]);

  // A clicked banner or line of the menu brings the window back on its item, selected in the inspector, or on the
  // Overview when it names none.
  useTauriEvent<Place | null>(OPEN_FAILURE, (place) => {
    if (place === null) {
      setView({ kind: "overview" });
      return;
    }
    switch (place.kind) {
      case "worktree":
        setView({ kind: "worktree", path: place.worktree });
        break;
      case "service":
        setView({ kind: "worktree", path: place.worktree });
        setSelection({ kind: "service", worktree: place.worktree, service: place.service });
        break;
      case "job":
        setView(place.worktree === null ? { kind: "outside" } : { kind: "worktree", path: place.worktree });
        setSelection({ kind: "job", id: place.id });
        break;
      default: {
        const unhandled: never = place;
        throw new Error(`unhandled place: ${JSON.stringify(unhandled)}`);
      }
    }
  });

  const repositories = snapshot.kind === "status" ? snapshot.report.repositories : [];
  const worktrees = repositories.flatMap((repository) => repository.worktrees);
  // A Worktree that is no longer reported leaves the Overview in its place.
  const current =
    view.kind === "worktree" ? (worktrees.find((worktree) => worktree.path === view.path) ?? null) : null;

  // The Daemon may stop reporting what was picked, an ended Job it evicted for one.
  const selected = snapshot.kind === "status" ? resolve(selection, snapshot.report) : null;
  // The Worktree the inspector offers to open, which the Worktree menu acts on.
  const inspected = selected === null ? current : selected.worktree;
  const failure = selected === null ? null : failureKeyOf(selected);
  useMenuContext(
    menuContext(
      selected,
      current,
      failure,
      snapshot.kind === "status" ? snapshot.needsYou.flatMap((line) => failureKey(line) ?? []) : [],
    ),
  );
  const release = (worktree: WorktreeStatus): void => {
    setReleasing({
      path: worktree.path,
      label: worktreeLabel(worktree),
      listed: worktree.listed,
      ended: endedIn(worktree),
      foreign: worktree.foreign ?? [],
    });
  };

  const onMarkSeen = (key: FailureKey): void => {
    // Rust answers with a new update, so a mark that could not be saved still shows.
    store.markSeen(key).catch(() => undefined);
  };

  // Restart, Stop and the opening of the Worktree are carried out by the inspector, which shows how they went.
  useTauriEvent<MenuChoice>(MENU_CHOICE, (choice) => {
    switch (choice) {
      case "overview":
        setView({ kind: "overview" });
        break;
      case "doctor":
        setView({ kind: "doctor" });
        break;
      case "orphans":
        setView({ kind: "orphans" });
        break;
      case "mark-seen":
        if (failure !== null) {
          onMarkSeen(failure);
        }
        break;
      case "release":
        if (inspected !== null) {
          release(inspected);
        }
        break;
      case "restart":
      case "stop":
      case "open-in-editor":
      case "open-in-terminal":
        break;
      // Rust carries these out without the window, so it never sends them.
      case "settings":
      case "refresh":
      case "copy-logs-command":
      case "copy-path":
        break;
      default: {
        const unhandled: never = choice;
        throw new Error(`unhandled choice: ${JSON.stringify(unhandled)}`);
      }
    }
  });

  const settings = view.kind === "settings";
  const checking = view.kind === "doctor";
  const orphans = view.kind === "orphans";
  const outside = view.kind === "outside";
  // Settings, the Orphans and the checks show nothing the inspector explains. The Jobs outside any Repository are picked
  // for it.
  const wide = settings || orphans || checking;
  // The Overview and a Worktree draw their own title line, with the controls of the toolbar at its end, so the toolbar
  // is left out.
  const titled = snapshot.kind === "status" && !checking;
  // The notice of a Procly release that drifted, which any screen but those two shows above its content and they show
  // under their title line.
  const banner =
    snapshot.kind === "status" ? (
      <ReleaseBanner
        store={doctor}
        onOpenDoctor={() => {
          setView({ kind: "doctor" });
        }}
      />
    ) : null;
  const doctorRun = useDoctor(doctor, null);
  // Where the Doctor takes the developer from a failed check: the Orphans, and the Foreign process the Status reports
  // for the Leases the `leased_ports` check names.
  const leasedPorts =
    doctorRun.kind === "report" || doctorRun.kind === "running"
      ? doctorRun.document?.checks.find((check) => check.name === "leased_ports" && check.status === "failed")
      : undefined;
  const processToShow =
    snapshot.kind === "status" && leasedPorts !== undefined
      ? foreignToShow(snapshot.report, leasedPorts.message)
      : null;
  const showProcess = (): void => {
    if (processToShow?.kind === "foreign") {
      setView({ kind: "worktree", path: processToShow.worktree });
      setSelection(processToShow);
    }
  };
  const orphanDryRun = useOrphanScan(orphanScan);
  const refresh = (): void => {
    // The poller answers with the effect, and a refresh asks for that answer now.
    store.refresh().catch(() => undefined);
  };
  const needEditor = (): void => {
    setView({ kind: "settings", focusEditor: true });
  };

  // Without a Daemon to read there is nothing for a sidebar or an inspector to hold, so the window is one screen.
  if (isBlocking(snapshot)) {
    return <DaemonStateScreen snapshot={snapshot} store={store} doctor={doctor} lastAnswer={lastAnswer} />;
  }

  return (
    <SelectionContext.Provider value={control}>
      <FailuresContext.Provider value={failures}>
        <div className={wide ? "window window-settings" : "window"}>
          <nav className="pane sidebar" aria-label="Worktrees">
            <div className="drag-strip" data-tauri-drag-region />
            <Sidebar
              snapshot={snapshot}
              repositories={repositories}
              current={current}
              settings={settings}
              checking={checking}
              orphans={orphans}
              outside={outside}
              outsideJobs={snapshot.kind === "status" ? snapshot.report.jobs_outside_repositories.length : 0}
              updatedAt={snapshot.kind === "status" ? snapshot.at : null}
              doctor={doctorRun}
              orphanScan={orphanDryRun}
              needsYou={snapshot.kind === "status" ? snapshot.needsYou : []}
              onSelect={(path) => {
                setView({ kind: "worktree", path });
              }}
              onOverview={() => {
                setView({ kind: "overview" });
              }}
              onOrphans={() => {
                setView({ kind: "orphans" });
              }}
              onOutside={() => {
                setView({ kind: "outside" });
              }}
              onDoctor={() => {
                setView({ kind: "doctor" });
              }}
              onSettings={() => {
                setView({ kind: "settings", focusEditor: false });
              }}
            />
          </nav>
          <main className="pane center">
            {view.kind === "settings" ? (
              <SettingsScreen focusEditor={view.focusEditor} />
            ) : view.kind === "orphans" && snapshot.kind === "status" ? (
              <OrphansScreen store={orphanScan} />
            ) : view.kind === "outside" && snapshot.kind === "status" ? (
              <OutsideRepositoriesScreen
                jobs={snapshot.report.jobs_outside_repositories}
                seen={snapshot.seen}
                controls={
                  <RefreshControls snapshot={snapshot} store={store} doctor={doctor} orphans={orphanScan} />
                }
              />
            ) : (
              <>
                {titled ? null : (
                  <Toolbar snapshot={snapshot} store={store} doctor={doctor} orphans={orphanScan} />
                )}
                {titled ? null : banner}
                {view.kind === "doctor" ? (
                  <DoctorScreen
                    store={doctor}
                    onShowOrphans={() => {
                      setView({ kind: "orphans" });
                    }}
                    onShowProcess={processToShow === null ? null : showProcess}
                  />
                ) : (
                  <Center
                    snapshot={snapshot}
                    current={current}
                    doctor={doctor}
                    banner={banner}
                    controls={
                      <RefreshControls
                        snapshot={snapshot}
                        store={store}
                        doctor={doctor}
                        orphans={orphanScan}
                      />
                    }
                    onRelease={release}
                    onNeedEditor={needEditor}
                  />
                )}
              </>
            )}
          </main>
          {wide ? null : (
            <aside className="pane inspector" aria-label="Inspector">
              <Inspector
                current={current}
                selected={selected}
                hint={outside ? "Select a Job." : "Select a Service or a Job to see its output."}
                at={snapshot.kind === "loading" ? 0 : snapshot.at}
                seen={snapshot.kind === "status" ? snapshot.seen : []}
                onMarkSeen={onMarkSeen}
                onChanged={refresh}
                onNeedEditor={needEditor}
              />
            </aside>
          )}
        </div>
        {releasing === null ? null : (
          <ReleaseSheet
            path={releasing.path}
            label={releasing.label}
            listed={releasing.listed}
            ended={releasing.ended}
            foreign={releasing.foreign}
            onClose={() => {
              setReleasing(null);
            }}
            onChanged={refresh}
          />
        )}
      </FailuresContext.Provider>
    </SelectionContext.Provider>
  );
}

interface RefreshProps {
  snapshot: DaemonSnapshot;
  store: DaemonStore;
  doctor: DoctorStore;
  orphans: OrphanScanStore;
}

/** When the poller last read a Status, and the button that asks it for another. */
function RefreshControls({ snapshot, store, doctor, orphans }: RefreshProps) {
  return (
    <>
      {snapshot.kind === "loading" ? null : (
        <span className="caption">{`Updated at ${formatMoment(snapshot.at, snapshot.at)}`}</span>
      )}
      <button
        type="button"
        className="button"
        onClick={() => {
          void store.refresh();
          void doctor.refresh();
          // The scan asks the Daemon, which a Daemon that does not answer cannot give.
          if (snapshot.kind === "status") {
            void orphans.run();
          }
        }}
      >
        Refresh
      </button>
    </>
  );
}

/** The controls above a screen that has no title line of its own yet. */
function Toolbar(props: RefreshProps) {
  return (
    <header className="toolbar">
      <RefreshControls {...props} />
    </header>
  );
}

/** "3 need you", "1 needs you": the size of Needs you, said on the Overview entry. */
function needsYouCount(count: number): string {
  return `${count} ${count === 1 ? "needs" : "need"} you`;
}

/** "1 Job", "2 Jobs": what the Outside Repositories entry holds. */
function jobsOutside(count: number): string {
  return `${count} ${count === 1 ? "Job" : "Jobs"}`;
}

function Sidebar({
  snapshot,
  repositories,
  current,
  settings,
  checking,
  orphans,
  outside,
  outsideJobs,
  updatedAt,
  doctor,
  orphanScan,
  needsYou,
  onSelect,
  onOverview,
  onOrphans,
  onOutside,
  onDoctor,
  onSettings,
}: {
  snapshot: DaemonSnapshot;
  repositories: RepositoryStatus[];
  current: WorktreeStatus | null;
  settings: boolean;
  checking: boolean;
  orphans: boolean;
  outside: boolean;
  /** How many Jobs started outside any Repository, so the entry says whether it holds something. */
  outsideJobs: number;
  /** When the poller last read a Status, `null` while none was read. */
  updatedAt: number | null;
  /** The last run of doctor for the machine, which the footer counts the problems of. */
  doctor: DoctorState;
  /** The last dry run of `Gc`, which the footer counts the Orphans of. */
  orphanScan: OrphanScan;
  /** The items of Needs you, the same list the Overview pins on top. */
  needsYou: NeedsYou[];
  onSelect: (path: string) => void;
  onOverview: () => void;
  onOrphans: () => void;
  onOutside: () => void;
  onDoctor: () => void;
  onSettings: () => void;
}) {
  return (
    <>
      <button
        type="button"
        className="row"
        aria-current={current === null && !settings && !orphans && !outside && !checking ? "true" : undefined}
        onClick={onOverview}
      >
        <span className="row-name">Overview</span>
        {needsYou.length === 0 ? null : <span className="row-count">{needsYouCount(needsYou.length)}</span>}
      </button>
      <SidebarWorktrees
        snapshot={snapshot}
        repositories={repositories}
        current={current}
        needsYou={needsYou}
        onSelect={onSelect}
      />
      <button
        type="button"
        className="row row-worktree"
        aria-current={outside ? "true" : undefined}
        onClick={onOutside}
      >
        <span className="row-line">
          <span className="row-name">Outside Repositories</span>
        </span>
        {outsideJobs === 0 ? null : <span className="row-detail">{jobsOutside(outsideJobs)}</span>}
      </button>
      <SidebarFooter
        updatedAt={updatedAt}
        doctor={doctor}
        orphans={orphanScan}
        checking={checking}
        orphansOpen={orphans}
        settings={settings}
        onDoctor={onDoctor}
        onOrphans={onOrphans}
        onSettings={onSettings}
      />
    </>
  );
}

function SidebarWorktrees({
  snapshot,
  repositories,
  current,
  needsYou,
  onSelect,
}: {
  snapshot: DaemonSnapshot;
  repositories: RepositoryStatus[];
  current: WorktreeStatus | null;
  needsYou: NeedsYou[];
  onSelect: (path: string) => void;
}) {
  if (snapshot.kind === "loading") {
    return <p className="caption">Reading the Daemon…</p>;
  }
  if (repositories.length === 0) {
    return <p className="caption">No Worktree.</p>;
  }
  return (
    <>
      {repositories.map((repository) => (
        <section key={repository.git_common_dir} className="group">
          <h2 className="caption">{repositoryLabel(repository.git_common_dir)}</h2>
          <ul className="list">
            {repository.worktrees.map((worktree) => (
              <li key={worktree.path}>
                <WorktreeEntry
                  worktree={worktree}
                  current={worktree.path === current?.path}
                  reason={reasonOf(needsYou, worktree.path)}
                  onSelect={onSelect}
                />
              </li>
            ))}
          </ul>
        </section>
      ))}
    </>
  );
}

/** A Worktree of the sidebar: its label, and under it why it needs the developer when something does. */
function WorktreeEntry({
  worktree,
  current,
  reason,
  onSelect,
}: {
  worktree: WorktreeStatus;
  /** Whether this Worktree is the one open in the center. */
  current: boolean;
  reason: Reason | null;
  onSelect: (path: string) => void;
}) {
  return (
    <button
      type="button"
      className="row row-worktree"
      title={worktreeTooltip(worktree)}
      aria-current={current ? "true" : undefined}
      onClick={() => {
        onSelect(worktree.path);
      }}
    >
      <span className="row-line">
        <span className="row-name">{worktreeLabel(worktree)}</span>
        <RemovedMark worktree={worktree} />
        {worktree.main ? <span className="caption row-legend">Main worktree</span> : null}
      </span>
      {reason === null ? null : (
        <span className={`row-reason tone-${reason.tone}`}>
          <GlyphMark glyph={reason.glyph} />
          <span className="row-reason-text">{reason.text}</span>
        </span>
      )}
    </button>
  );
}

function Center({
  snapshot,
  current,
  doctor,
  controls,
  banner,
  onRelease,
  onNeedEditor,
}: {
  snapshot: Exclude<DaemonSnapshot, BlockingSnapshot>;
  current: WorktreeStatus | null;
  doctor: DoctorStore;
  /** The end of the title line of the Overview or of a Worktree: when the answer was read, and Refresh. */
  controls: ReactNode;
  /** The notice the Overview or a Worktree draws under its title line, nothing when there is none. */
  banner: ReactNode;
  onRelease: (worktree: WorktreeStatus) => void;
  onNeedEditor: () => void;
}) {
  switch (snapshot.kind) {
    case "loading":
      return <p className="caption">Reading the Daemon…</p>;
    case "status":
      return current === null ? (
        <Overview
          repositories={snapshot.report.repositories}
          at={snapshot.at}
          needsYou={snapshot.needsYou}
          seen={snapshot.seen}
          controls={controls}
          banner={banner}
        />
      ) : (
        <WorktreeScreen
          worktree={current}
          at={snapshot.at}
          seen={snapshot.seen}
          doctor={doctor}
          controls={controls}
          banner={banner}
          onRelease={onRelease}
          onNeedEditor={onNeedEditor}
        />
      );
    default: {
      const unhandled: never = snapshot;
      throw new Error(`unhandled snapshot: ${JSON.stringify(unhandled)}`);
    }
  }
}

/** Whether a line of Needs you is this very Service, which is then listed there and not again among the Services. */
function pinnedIn(needsYou: NeedsYou[], worktree: WorktreeStatus, service: ServiceStatus): boolean {
  return needsYou.some(
    (line) =>
      line.kind === "service" &&
      line.worktree === worktree.path &&
      line.service === service.service &&
      line.started_at === service.started_at,
  );
}

/**
 * The title line with when the answer was read, then "Needs you" pinned on top, then the Services of every Worktree in
 * one list, each with the Worktree it runs in and its port, except those Needs you already holds: its Process services
 * and Groups, then its Isolated Compose services. `at` is when the answer was read, the day the times are compared
 * with. When nothing runs and nothing needs the developer, the Overview says how to have the agents start something.
 */
function Overview({
  repositories,
  at,
  needsYou,
  seen,
  controls,
  banner,
}: {
  repositories: RepositoryStatus[];
  at: number;
  needsYou: NeedsYou[];
  seen: FailureKey[];
  controls: ReactNode;
  banner: ReactNode;
}) {
  const worktrees = repositories.flatMap((repository) => repository.worktrees);
  const rows = worktrees.flatMap((worktree) => [
    ...worktree.services
      .filter((service) => !pinnedIn(needsYou, worktree, service))
      .map((service) => (
        <ServiceRow
          key={`${worktree.path}:${service.service}`}
          service={service}
          worktreePath={worktree.path}
          worktree={worktree}
          at={at}
          seen={seen}
        />
      )),
    ...(worktree.containers ?? []).map((container) => (
      <ContainerRow
        key={`${worktree.path}:${container.service}`}
        container={container}
        worktreePath={worktree.path}
        worktree={worktree}
      />
    )),
  ]);
  const nothingRuns =
    needsYou.length === 0 &&
    worktrees.every((worktree) => worktree.services.length === 0 && (worktree.containers ?? []).length === 0);
  return (
    <section aria-label="Overview">
      <TitleBar title="Overview">{controls}</TitleBar>
      {banner}
      {nothingRuns ? (
        <NothingRunsYet />
      ) : (
        <>
          <NeedsYouSection lines={needsYou} worktrees={worktrees} />
          {rows.length === 0 ? null : (
            <div className="services">
              <h3 className="subtitle">
                <span className="row-name">Services</span>{" "}
                <span className="caption">every Worktree, without those above</span>
              </h3>
              <ul className="list">{rows}</ul>
            </div>
          )}
        </>
      )}
    </section>
  );
}

/** The Overview of a Daemon that knows no Service: what appears here, and the command that steers the agents to Procly. */
function NothingRunsYet() {
  return (
    <div className="state-column overview-empty">
      <BrandMark />
      <h3 className="title">Nothing is running yet</h3>
      <p className="muted">
        Your coding agents start Services and Jobs through Procly, and they appear here. To steer them to
        Procly, install the Guard and the Rules block in a Repository.
      </p>
      <CopyBox text="procly agent install" />
    </div>
  );
}

/** What the developer has to look at, pinned on top of the Overview: nothing is drawn when nothing needs them. */
function NeedsYouSection({ lines, worktrees }: { lines: NeedsYou[]; worktrees: WorktreeStatus[] }) {
  if (lines.length === 0) {
    return null;
  }
  return (
    <section aria-label="Needs you" className="needs-you">
      <h3 className="subtitle">
        <span className="row-name">Needs you</span> <span className="caption">{lines.length}</span>
      </h3>
      <ul className="list">
        {lines.map((line) => (
          <NeedsYouLine key={lineKey(line)} line={line} worktrees={worktrees} />
        ))}
      </ul>
    </section>
  );
}

function lineKey(line: NeedsYou): string {
  switch (line.kind) {
    case "service":
      return `service:${line.worktree}:${line.service}:${line.started_at}`;
    case "job":
      return `job:${line.id}`;
    case "foreign":
      return `foreign:${line.worktree}:${foreignId(line.process)}`;
    default: {
      const unhandled: never = line;
      throw new Error(`unhandled line: ${JSON.stringify(unhandled)}`);
    }
  }
}

/** The item a line of Needs you picks for the inspector. */
function targetOfLine(line: NeedsYou): Selection {
  switch (line.kind) {
    case "service":
      return { kind: "service", worktree: line.worktree, service: line.service };
    case "job":
      return { kind: "job", id: line.id };
    case "foreign":
      return { kind: "foreign", worktree: line.worktree, process: foreignId(line.process) };
    default: {
      const unhandled: never = line;
      throw new Error(`unhandled line: ${JSON.stringify(unhandled)}`);
    }
  }
}

/**
 * A failure is picked like any row, and its inspector holds Mark as Seen. A Foreign process is picked too, and its
 * inspector gives the command to stop it. A line that is Seen leaves Needs you, so a line here is never dimmed.
 */
function NeedsYouLine({ line, worktrees }: { line: NeedsYou; worktrees: WorktreeStatus[] }) {
  const target = targetOfLine(line);
  const whereOf = (path: string): { where: ReactNode; whereTitle: string } => {
    const worktree = worktrees.find((candidate) => candidate.path === path);
    return worktree === undefined ? { where: basename(path), whereTitle: path } : worktreeWhere(worktree);
  };
  switch (line.kind) {
    case "service": {
      const worktree = worktrees.find((candidate) => candidate.path === line.worktree);
      const port = worktree?.services.find((service) => service.service === line.service)?.port ?? null;
      return (
        <ItemRow
          target={target}
          seen={false}
          // A line of "Needs you" that is not a Foreign process is a failure: Rust put it there.
          state={serviceEndWord(line.end, true)}
          name={line.service}
          {...whereOf(line.worktree)}
          ports={port === null ? [] : [port]}
        />
      );
    }
    case "job":
      return (
        <ItemRow
          target={target}
          seen={false}
          state={jobEndWord(line.end, true)}
          name={line.command}
          mono
          {...(line.worktree === null ? { where: "outside Repositories" } : whereOf(line.worktree))}
        >
          <span className="caption mono">{line.id}</span>
        </ItemRow>
      );
    case "foreign":
      return (
        <ItemRow
          target={target}
          seen={false}
          state={foreignWord()}
          name={line.process.kind === "process" ? line.process.command : line.process.name}
          mono
          {...whereOf(line.worktree)}
          ports={line.process.ports}
        />
      );
    default: {
      const unhandled: never = line;
      throw new Error(`unhandled line: ${JSON.stringify(unhandled)}`);
    }
  }
}

function Inspector({
  current,
  selected,
  hint,
  at,
  seen,
  onMarkSeen,
  onChanged,
  onNeedEditor,
}: {
  current: WorktreeStatus | null;
  selected: Selected | null;
  /** What the inspector says while nothing is picked and no Worktree is open. */
  hint: string;
  at: number;
  seen: FailureKey[];
  onMarkSeen: (key: FailureKey) => void;
  onChanged: () => void;
  onNeedEditor: () => void;
}) {
  if (selected !== null) {
    return (
      <>
        <SelectedPanel
          key={selectedKey(selected)}
          selected={selected}
          at={at}
          seen={seen}
          onMarkSeen={onMarkSeen}
          onChanged={onChanged}
        />
        {selected.worktree === null ? null : (
          <WorktreeActions
            key={selected.worktree.path}
            path={selected.worktree.path}
            // The inspector of a Foreign process draws no Reveal in Finder, the Worktree menu still opens its Worktree.
            buttons={selected.kind !== "foreign"}
            onNeedEditor={onNeedEditor}
          />
        )}
      </>
    );
  }
  if (current === null) {
    return <p className="hint">{hint}</p>;
  }
  return (
    <>
      <WorktreeFacts current={current} />
      <WorktreeActions key={current.path} path={current.path} onNeedEditor={onNeedEditor} />
    </>
  );
}

/**
 * Reveals the Worktree in the Finder, and opens it in the editor or the terminal the settings name when Open in Editor
 * or Open in Terminal of the menu bar ask for it, so that a failure shows here. The buttons that open it live in the
 * title line of the Worktree.
 */
function WorktreeActions({
  path,
  buttons = true,
  onNeedEditor,
}: {
  path: string;
  /** Whether the button is drawn. Without it the menu bar still opens the Worktree, and a failure still shows. */
  buttons?: boolean;
  onNeedEditor: () => void;
}) {
  const { error, run } = useOpening(onNeedEditor);

  useTauriEvent<MenuChoice>(MENU_CHOICE, (choice) => {
    if (choice === "open-in-editor") {
      run(openWorktree(path, "editor"));
    } else if (choice === "open-in-terminal") {
      run(openWorktree(path, "terminal"));
    }
  });

  return (
    <div className="worktree-actions">
      {buttons ? (
        <button
          type="button"
          className="button"
          onClick={() => {
            run(revealWorktree(path));
          }}
        >
          Reveal in Finder
        </button>
      ) : null}
      {error === null ? null : (
        <p role="alert" className="error">
          {error}
        </p>
      )}
    </div>
  );
}

function WorktreeFacts({ current }: { current: WorktreeStatus }) {
  return (
    <dl className="facts">
      <WorktreeNames worktree={current} />
      <dt className="caption">Path</dt>
      <dd className="mono">{current.path}</dd>
      <dt className="caption">Kind</dt>
      <dd>{current.main ? "Main worktree" : "Worktree"}</dd>
      <dt className="caption">Services</dt>
      <dd>{current.services.length}</dd>
      <dt className="caption">Jobs</dt>
      <dd>{current.jobs.length}</dd>
    </dl>
  );
}
