import { listen } from "@tauri-apps/api/event";
import { useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";

import { DaemonNotRunning, DaemonOfAnotherVersion } from "./DaemonScreens";
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
import { repositoryLabel, worktreeLabel, worktreeTooltip } from "./labels";
import { MENU_CHOICE, menuContext, useMenuContext, type MenuChoice } from "./menu";
import { failureKey, FailuresContext, failuresOf, reasonOf, type Reason } from "./needs-you";
import { createOrphanScanStore, useOrphanScan, type OrphanScan, type OrphanScanStore } from "./orphan-scan";
import { OrphansScreen } from "./OrphansScreen";
import { endedIn, type Ended } from "./release";
import { ReleaseSheet } from "./ReleaseSheet";
import { SelectableRow, SelectionContext } from "./selectable";
import { resolve, selectedKey, type Selected, type Selection } from "./selection";
import { failureKeyOf, SelectedPanel, WorktreeNames } from "./SelectedPanel";
import { SettingsScreen } from "./SettingsScreen";
import { SidebarFooter } from "./SidebarFooter";
import { failureOf, OPEN_FAILURE, OPEN_SETTINGS, openWorktree, revealWorktree, type Place } from "./settings";
import { State } from "./StateMark";
import type { RepositoryStatus } from "./protocol/RepositoryStatus";
import type { WorktreeStatus } from "./protocol/WorktreeStatus";
import { ContainerRow, RemovedMark, ServiceRow } from "./Rows";
import { foreignWord, jobEndWord, serviceEndWord } from "./state";
import { formatMoment } from "./time";
import { OutsideRepositoriesScreen, WorktreeScreen } from "./WorktreeScreen";

/**
 * What the center shows: the Overview of every Service, one Worktree, the Jobs outside any Repository, the Orphans,
 * Doctor, or Settings. The Orphans and Settings hide the inspector. `focusEditor`
 * sends the developer to the editor field, because "Open in editor" found none set.
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
  } | null>(null);

  // The poller of the app keeps the snapshot current, the window only follows it.
  useEffect(() => store.start(), [store]);

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
  // Settings and the Orphans show nothing the inspector explains. The Jobs outside any Repository are picked for it.
  const wide = settings || orphans;
  const doctorRun = useDoctor(doctor, null);
  const orphanDryRun = useOrphanScan(orphanScan);
  const refresh = (): void => {
    // The poller answers with the effect, and a refresh asks for that answer now.
    store.refresh().catch(() => undefined);
  };

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
              />
            ) : (
              <>
                <Toolbar snapshot={snapshot} store={store} doctor={doctor} orphans={orphanScan} />
                {snapshot.kind === "status" ? (
                  <ReleaseBanner
                    store={doctor}
                    onOpenDoctor={() => {
                      setView({ kind: "doctor" });
                    }}
                  />
                ) : null}
                {view.kind === "doctor" ? (
                  <DoctorScreen store={doctor} />
                ) : (
                  <Center
                    snapshot={snapshot}
                    current={current}
                    store={store}
                    doctor={doctor}
                    onRelease={release}
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
                hint={outside ? "Select a Job." : "Select a Worktree."}
                at={snapshot.kind === "loading" ? 0 : snapshot.at}
                seen={snapshot.kind === "status" ? snapshot.seen : []}
                onMarkSeen={onMarkSeen}
                onChanged={refresh}
                onNeedEditor={() => {
                  setView({ kind: "settings", focusEditor: true });
                }}
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

function Toolbar({
  snapshot,
  store,
  doctor,
  orphans,
}: {
  snapshot: DaemonSnapshot;
  store: DaemonStore;
  doctor: DoctorStore;
  orphans: OrphanScanStore;
}) {
  return (
    <header className="toolbar">
      {snapshot.kind === "loading" ? null : (
        <span className="caption">{`Updated ${formatMoment(snapshot.at, snapshot.at)}`}</span>
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
    </header>
  );
}

/** "3 need you", "1 needs you": the size of Needs you, said on the Overview entry. */
function needsYouCount(count: number): string {
  return `${count} ${count === 1 ? "needs" : "need"} you`;
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
      <button type="button" className="row" aria-current={outside ? "true" : undefined} onClick={onOutside}>
        <span className="row-name">Outside Repositories</span>
        {outsideJobs === 0 ? null : <span className="caption">{outsideJobs}</span>}
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
                  open={worktree.path === current?.path}
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
  open,
  reason,
  onSelect,
}: {
  worktree: WorktreeStatus;
  open: boolean;
  reason: Reason | null;
  onSelect: (path: string) => void;
}) {
  return (
    <button
      type="button"
      className="row row-worktree"
      title={worktreeTooltip(worktree)}
      aria-current={open ? "true" : undefined}
      onClick={() => {
        onSelect(worktree.path);
      }}
    >
      <span className="row-line">
        <span className="row-name">{worktreeLabel(worktree)}</span>
        <RemovedMark worktree={worktree} />
        {worktree.main ? <span className="caption">main</span> : null}
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
  store,
  doctor,
  onRelease,
}: {
  snapshot: DaemonSnapshot;
  current: WorktreeStatus | null;
  store: DaemonStore;
  doctor: DoctorStore;
  onRelease: (worktree: WorktreeStatus) => void;
}) {
  switch (snapshot.kind) {
    case "loading":
      return <p className="caption">Reading the Daemon…</p>;
    case "no-daemon":
      return <DaemonNotRunning store={store} doctor={doctor} />;
    case "protocol-mismatch":
      return <DaemonOfAnotherVersion daemon={snapshot.daemon} app={snapshot.app} store={store} />;
    case "failure":
      return (
        <section>
          <h2 className="title">The Daemon did not answer</h2>
          <p className="muted">{snapshot.message}</p>
        </section>
      );
    case "status":
      return current === null ? (
        <Overview
          repositories={snapshot.report.repositories}
          at={snapshot.at}
          needsYou={snapshot.needsYou}
          seen={snapshot.seen}
        />
      ) : (
        <WorktreeScreen
          worktree={current}
          at={snapshot.at}
          seen={snapshot.seen}
          doctor={doctor}
          onRelease={onRelease}
        />
      );
    default: {
      const unhandled: never = snapshot;
      throw new Error(`unhandled snapshot: ${JSON.stringify(unhandled)}`);
    }
  }
}

/**
 * "Needs you" pinned on top, then every Service of every Worktree, grouped under the Worktree, with its port: its
 * Process services and Groups, then its Isolated Compose services. `at` is when the answer was read, the day the times
 * are compared with.
 */
function Overview({
  repositories,
  at,
  needsYou,
  seen,
}: {
  repositories: RepositoryStatus[];
  at: number;
  needsYou: NeedsYou[];
  seen: FailureKey[];
}) {
  const groups = repositories
    .map((repository) => ({
      repository,
      worktrees: repository.worktrees.filter(
        (worktree) => worktree.services.length > 0 || (worktree.containers ?? []).length > 0,
      ),
    }))
    .filter((group) => group.worktrees.length > 0);
  return (
    <section aria-label="Overview">
      <h2 className="title">Overview</h2>
      <NeedsYouSection
        lines={needsYou}
        worktrees={repositories.flatMap((repository) => repository.worktrees)}
      />
      {groups.length === 0 ? <p className="muted">No Service is known.</p> : null}
      {groups.map(({ repository, worktrees }) => (
        <div key={repository.git_common_dir} className="group">
          <p className="caption">{repositoryLabel(repository.git_common_dir)}</p>
          {worktrees.map((worktree) => (
            <div key={worktree.path}>
              <h3 className="subtitle" title={worktreeTooltip(worktree)}>
                <span className="row-name">{worktreeLabel(worktree)}</span>
                <RemovedMark worktree={worktree} />
              </h3>
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
                {(worktree.containers ?? []).map((container) => (
                  <ContainerRow key={container.service} container={container} worktreePath={worktree.path} />
                ))}
              </ul>
            </div>
          ))}
        </div>
      ))}
    </section>
  );
}

/** What the developer has to look at, pinned on top of the Overview: nothing is drawn when nothing needs them. */
function NeedsYouSection({ lines, worktrees }: { lines: NeedsYou[]; worktrees: WorktreeStatus[] }) {
  if (lines.length === 0) {
    return null;
  }
  const labelOf = (path: string): string => {
    const worktree = worktrees.find((candidate) => candidate.path === path);
    return worktree === undefined ? path : worktreeLabel(worktree);
  };
  return (
    <section aria-label="Needs you" className="needs-you">
      <h3 className="subtitle">
        <span className="row-name">Needs you</span> <span className="caption">{lines.length}</span>
      </h3>
      <ul className="list">
        {lines.map((line) => (
          <NeedsYouLine key={lineKey(line)} line={line} labelOf={labelOf} />
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
      return `foreign:${line.worktree}:${line.process.kind === "process" ? line.process.pid : line.process.name}`;
    default: {
      const unhandled: never = line;
      throw new Error(`unhandled line: ${JSON.stringify(unhandled)}`);
    }
  }
}

/** The item a line of Needs you picks for the inspector, nothing for a Foreign process, which has no inspector yet. */
function targetOfLine(line: NeedsYou): Selection | null {
  switch (line.kind) {
    case "service":
      return { kind: "service", worktree: line.worktree, service: line.service };
    case "job":
      return { kind: "job", id: line.id };
    case "foreign":
      return null;
    default: {
      const unhandled: never = line;
      throw new Error(`unhandled line: ${JSON.stringify(unhandled)}`);
    }
  }
}

/**
 * A failure is picked like any row, and its inspector holds Mark as Seen. A line that is Seen leaves Needs you, so a
 * line here is never dimmed.
 */
function NeedsYouLine({ line, labelOf }: { line: NeedsYou; labelOf: (path: string) => string }) {
  const target = targetOfLine(line);
  return target === null ? (
    <li className="item">
      <NeedsYouWhat line={line} labelOf={labelOf} />
    </li>
  ) : (
    <SelectableRow target={target} seen={false}>
      <NeedsYouWhat line={line} labelOf={labelOf} />
    </SelectableRow>
  );
}

function NeedsYouWhat({ line, labelOf }: { line: NeedsYou; labelOf: (path: string) => string }) {
  switch (line.kind) {
    case "service":
      return (
        <>
          <span className="item-name">{line.service}</span>
          {/* A line of "Needs you" that is not a Foreign process is a failure: Rust put it there. */}
          <State {...serviceEndWord(line.end, true)} />
          <span className="caption">{labelOf(line.worktree)}</span>
        </>
      );
    case "job":
      return (
        <>
          <span className="item-name mono">{line.command}</span>
          <State {...jobEndWord(line.end, true)} />
          <span className="caption mono">{line.id}</span>
          {line.worktree === null ? null : <span className="caption">{labelOf(line.worktree)}</span>}
        </>
      );
    case "foreign":
      return (
        <>
          <State {...foreignWord()} />
          <span className="item-name mono">
            {line.process.kind === "process" ? line.process.command : line.process.name}
          </span>
          {line.process.ports.length === 0 ? null : (
            <span className="caption">{line.process.ports.map((port) => `:${port}`).join(" ")}</span>
          )}
          <span className="caption">{labelOf(line.worktree)}</span>
        </>
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
            onNeedEditor={onNeedEditor}
          />
        )}
      </>
    );
  }
  if (current === null) {
    return <p className="caption">{hint}</p>;
  }
  return (
    <>
      <WorktreeFacts current={current} />
      <WorktreeActions key={current.path} path={current.path} onNeedEditor={onNeedEditor} />
    </>
  );
}

/**
 * Opens the Worktree in the editor or the terminal the settings name, or reveals it in the Finder. Open in Editor and
 * Open in Terminal of the menu bar open it here, so that a failure shows under the buttons.
 */
function WorktreeActions({ path, onNeedEditor }: { path: string; onNeedEditor: () => void }) {
  const [error, setError] = useState<string | null>(null);

  function run(action: Promise<void>): void {
    setError(null);
    action.catch((reason: unknown) => {
      const failure = failureOf(reason);
      if (failure.kind === "no_editor") {
        onNeedEditor();
      } else {
        setError(failure.message);
      }
    });
  }

  useTauriEvent<MenuChoice>(MENU_CHOICE, (choice) => {
    if (choice === "open-in-editor") {
      run(openWorktree(path, "editor"));
    } else if (choice === "open-in-terminal") {
      run(openWorktree(path, "terminal"));
    }
  });

  return (
    <div className="worktree-actions">
      <button
        type="button"
        className="button"
        onClick={() => {
          run(openWorktree(path, "editor"));
        }}
      >
        Open in editor
      </button>
      <button
        type="button"
        className="button"
        onClick={() => {
          run(openWorktree(path, "terminal"));
        }}
      >
        Open in terminal
      </button>
      <button
        type="button"
        className="button"
        onClick={() => {
          run(revealWorktree(path));
        }}
      >
        Reveal in Finder
      </button>
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
