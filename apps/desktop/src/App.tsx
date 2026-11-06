import { listen } from "@tauri-apps/api/event";
import { useCallback, useEffect, useMemo, useState, useSyncExternalStore } from "react";

import { DaemonNotRunning, DaemonOfAnotherVersion } from "./DaemonScreens";
import type { DaemonSnapshot, DaemonStore, FailureKey, NeedsYou } from "./daemon";
import { createDoctorStore, REFRESH_REQUESTED, useTauriEvent, type DoctorStore } from "./doctor";
import { DoctorScreen, ReleaseBanner, WorktreeChecks } from "./DoctorScreen";
import { repositoryLabel, worktreeLabel, worktreeTooltip } from "./labels";
import { failureKey, isSeen } from "./needs-you";
import { OrphansScreen } from "./OrphansScreen";
import { SelectableRow, SelectionContext } from "./selectable";
import { resolve, selectedKey, type Selected, type Selection } from "./selection";
import { SelectedPanel } from "./SelectedPanel";
import { SettingsScreen } from "./SettingsScreen";
import { failureOf, OPEN_FAILURE, OPEN_SETTINGS, openWorktree, revealWorktree } from "./settings";
import { State } from "./StateMark";
import type { ContainerStatus } from "./protocol/ContainerStatus";
import type { JobStatus } from "./protocol/JobStatus";
import type { RepositoryStatus } from "./protocol/RepositoryStatus";
import type { ServiceStatus } from "./protocol/ServiceStatus";
import type { WorktreeStatus } from "./protocol/WorktreeStatus";
import { jobEndWord, jobWord, serviceEndWord, serviceWord } from "./state";
import { formatMoment } from "./time";

/**
 * What the center shows: the Overview of every Service, one Worktree, the Orphans, Doctor, or Settings. Orphans and
 * Settings hide the inspector. `focusEditor` sends the developer to the editor field, because "Open in editor" found
 * none set.
 */
type View =
  | { kind: "overview" }
  | { kind: "worktree"; path: string }
  | { kind: "orphans" }
  | { kind: "doctor" }
  | { kind: "settings"; focusEditor: boolean };

/** `doctor` is the store of the checks, a window makes its own unless a test hands it one. */
export function App({ store, doctor: given }: { store: DaemonStore; doctor?: DoctorStore }) {
  const snapshot = useSyncExternalStore(store.subscribe, store.getSnapshot);
  const [view, setCurrentView] = useState<View>({ kind: "overview" });
  const [selection, setSelection] = useState<Selection | null>(null);
  // Another screen shows another set of items, so what was picked in the one before is no longer on show.
  const setView = useCallback((next: View): void => {
    setCurrentView(next);
    setSelection(null);
  }, []);
  const control = useMemo(() => ({ selection, select: setSelection }), [selection]);
  const [doctor] = useState(() => given ?? createDoctorStore());

  // The poller of the app keeps the snapshot current, the window only follows it.
  useEffect(() => store.start(), [store]);

  // Doctor runs when the app enters "Daemon not running", not on each tick that finds it still not running.
  const notRunning = snapshot.kind === "no-daemon";
  useEffect(() => {
    if (notRunning) {
      void doctor.run(null);
    }
  }, [doctor, notRunning]);

  // The menu refreshes without the window, so Rust says so.
  useTauriEvent(REFRESH_REQUESTED, () => {
    void doctor.refresh();
  });

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent): void => {
      if ((event.metaKey || event.ctrlKey) && event.key === "0") {
        setView({ kind: "overview" });
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [setView]);

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

  // A clicked banner brings the window back on the Worktree of its failure, or on the Overview when it names none.
  useTauriEvent<string | null>(OPEN_FAILURE, (worktree) => {
    setView(worktree === null ? { kind: "overview" } : { kind: "worktree", path: worktree });
  });

  const repositories = snapshot.kind === "status" ? snapshot.report.repositories : [];
  const worktrees = repositories.flatMap((repository) => repository.worktrees);
  // A Worktree that is no longer reported leaves the Overview in its place.
  const current =
    view.kind === "worktree" ? (worktrees.find((worktree) => worktree.path === view.path) ?? null) : null;

  // The Daemon may stop reporting what was picked, an ended Job it evicted for one.
  const selected = snapshot.kind === "status" ? resolve(selection, snapshot.report) : null;

  const settings = view.kind === "settings";
  const checking = view.kind === "doctor";
  const orphans = view.kind === "orphans";
  // A screen that is not about one Worktree has no inspector.
  const wide = settings || orphans;

  return (
    <SelectionContext.Provider value={control}>
      <div className={wide ? "window window-settings" : "window"}>
        <nav className="pane sidebar" aria-label="Worktrees">
          <h1 className="pane-title">Procly</h1>
          <Sidebar
            snapshot={snapshot}
            repositories={repositories}
            current={current}
            settings={settings}
            checking={checking}
            orphans={orphans}
            onSelect={(path) => {
              setView({ kind: "worktree", path });
            }}
            onOverview={() => {
              setView({ kind: "overview" });
            }}
            onOrphans={() => {
              setView({ kind: "orphans" });
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
            <OrphansScreen />
          ) : (
            <>
              <Toolbar snapshot={snapshot} store={store} doctor={doctor} />
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
                <Center snapshot={snapshot} current={current} store={store} doctor={doctor} />
              )}
            </>
          )}
        </main>
        {wide ? null : (
          <aside className="pane inspector" aria-label="Inspector">
            <Inspector
              current={current}
              selected={selected}
              at={snapshot.kind === "loading" ? 0 : snapshot.at}
              onNeedEditor={() => {
                setView({ kind: "settings", focusEditor: true });
              }}
            />
          </aside>
        )}
      </div>
    </SelectionContext.Provider>
  );
}

function Toolbar({
  snapshot,
  store,
  doctor,
}: {
  snapshot: DaemonSnapshot;
  store: DaemonStore;
  doctor: DoctorStore;
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
        }}
      >
        Refresh
      </button>
    </header>
  );
}

function Sidebar({
  snapshot,
  repositories,
  current,
  settings,
  checking,
  orphans,
  onSelect,
  onOverview,
  onOrphans,
  onDoctor,
  onSettings,
}: {
  snapshot: DaemonSnapshot;
  repositories: RepositoryStatus[];
  current: WorktreeStatus | null;
  settings: boolean;
  checking: boolean;
  orphans: boolean;
  onSelect: (path: string) => void;
  onOverview: () => void;
  onOrphans: () => void;
  onDoctor: () => void;
  onSettings: () => void;
}) {
  return (
    <>
      <button
        type="button"
        className="row"
        aria-current={current === null && !settings && !orphans && !checking ? "true" : undefined}
        onClick={onOverview}
      >
        <span className="row-name">Overview</span>
        <span className="caption">⌘0</span>
      </button>
      <SidebarWorktrees
        snapshot={snapshot}
        repositories={repositories}
        current={current}
        onSelect={onSelect}
      />
      <button
        type="button"
        className="row sidebar-foot"
        aria-current={checking ? "true" : undefined}
        onClick={onDoctor}
      >
        <span className="row-name">Doctor</span>
      </button>
      <button type="button" className="row" aria-current={orphans ? "true" : undefined} onClick={onOrphans}>
        <span className="row-name">Orphans</span>
      </button>
      <button type="button" className="row" aria-current={settings ? "true" : undefined} onClick={onSettings}>
        <span className="row-name">Settings</span>
        <span className="caption">⌘,</span>
      </button>
    </>
  );
}

function SidebarWorktrees({
  snapshot,
  repositories,
  current,
  onSelect,
}: {
  snapshot: DaemonSnapshot;
  repositories: RepositoryStatus[];
  current: WorktreeStatus | null;
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
                <button
                  type="button"
                  className="row"
                  title={worktreeTooltip(worktree)}
                  aria-current={worktree.path === current?.path ? "true" : undefined}
                  onClick={() => {
                    onSelect(worktree.path);
                  }}
                >
                  <span className="row-name">{worktreeLabel(worktree)}</span>
                  <RemovedMark worktree={worktree} />
                  {worktree.main ? <span className="caption">main</span> : null}
                </button>
              </li>
            ))}
          </ul>
        </section>
      ))}
    </>
  );
}

/** Git no longer lists the Worktree, though the Daemon still runs something in it. */
function RemovedMark({ worktree }: { worktree: WorktreeStatus }) {
  return worktree.listed ? null : <span className="mark">Removed</span>;
}

function Center({
  snapshot,
  current,
  store,
  doctor,
}: {
  snapshot: DaemonSnapshot;
  current: WorktreeStatus | null;
  store: DaemonStore;
  doctor: DoctorStore;
}) {
  const onMarkSeen = (key: FailureKey): void => {
    // Rust answers with a new update, so a mark that could not be saved still shows.
    store.markSeen(key).catch(() => undefined);
  };
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
          onMarkSeen={onMarkSeen}
        />
      ) : (
        <WorktreeView worktree={current} at={snapshot.at} seen={snapshot.seen} doctor={doctor} />
      );
    default: {
      const unhandled: never = snapshot;
      throw new Error(`unhandled snapshot: ${JSON.stringify(unhandled)}`);
    }
  }
}

/**
 * "Needs you" pinned on top, then every Service of every Worktree, grouped under the Worktree, with its port. `at` is
 * when the answer was read, the day the times are compared with.
 */
function Overview({
  repositories,
  at,
  needsYou,
  seen,
  onMarkSeen,
}: {
  repositories: RepositoryStatus[];
  at: number;
  needsYou: NeedsYou[];
  seen: FailureKey[];
  onMarkSeen: (key: FailureKey) => void;
}) {
  const groups = repositories
    .map((repository) => ({
      repository,
      worktrees: repository.worktrees.filter((worktree) => worktree.services.length > 0),
    }))
    .filter((group) => group.worktrees.length > 0);
  return (
    <section aria-label="Overview">
      <h2 className="title">Overview</h2>
      <NeedsYouSection
        lines={needsYou}
        worktrees={repositories.flatMap((repository) => repository.worktrees)}
        onMarkSeen={onMarkSeen}
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
              </ul>
            </div>
          ))}
        </div>
      ))}
    </section>
  );
}

/** What the developer has to look at, pinned on top of the Overview: nothing is drawn when nothing needs them. */
function NeedsYouSection({
  lines,
  worktrees,
  onMarkSeen,
}: {
  lines: NeedsYou[];
  worktrees: WorktreeStatus[];
  onMarkSeen: (key: FailureKey) => void;
}) {
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
          <NeedsYouLine key={lineKey(line)} line={line} labelOf={labelOf} onMarkSeen={onMarkSeen} />
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

function NeedsYouLine({
  line,
  labelOf,
  onMarkSeen,
}: {
  line: NeedsYou;
  labelOf: (path: string) => string;
  onMarkSeen: (key: FailureKey) => void;
}) {
  const key = failureKey(line);
  return (
    <li className="item">
      <NeedsYouWhat line={line} labelOf={labelOf} />
      {key === null ? null : (
        <button
          type="button"
          className="button"
          onClick={() => {
            onMarkSeen(key);
          }}
        >
          Mark as Seen
        </button>
      )}
    </li>
  );
}

function NeedsYouWhat({ line, labelOf }: { line: NeedsYou; labelOf: (path: string) => string }) {
  switch (line.kind) {
    case "service":
      return (
        <>
          <span className="item-name">{line.service}</span>
          <State {...serviceEndWord(line.end)} />
          <span className="caption">{labelOf(line.worktree)}</span>
        </>
      );
    case "job":
      return (
        <>
          <span className="item-name mono">{line.command}</span>
          <State {...jobEndWord(line.end)} />
          <span className="caption mono">{line.id}</span>
          {line.worktree === null ? null : <span className="caption">{labelOf(line.worktree)}</span>}
        </>
      );
    case "foreign":
      return (
        <>
          <span className="mark">Foreign</span>
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

/** The mark of a failure the developer saw: its row stays, dimmed, and says so in words as well. */
function SeenMark({ seen }: { seen: boolean }) {
  return seen ? <span className="mark">Seen</span> : null;
}

function ServiceRow({
  service,
  worktreePath,
  at,
  seen,
}: {
  service: ServiceStatus;
  worktreePath: string;
  at: number;
  seen: FailureKey[];
}) {
  const marked = isSeen(seen, {
    kind: "service",
    worktree: worktreePath,
    service: service.service,
    started_at: service.started_at,
  });
  return (
    <SelectableRow
      target={{ kind: "service", worktree: worktreePath, service: service.service }}
      seen={marked}
    >
      <span className="item-name">{service.service}</span>
      <State {...serviceWord(service)} />
      {service.port === null ? null : <span className="caption">{`:${service.port}`}</span>}
      <span className="caption">{formatMoment(service.started_at, at)}</span>
      <SeenMark seen={marked} />
    </SelectableRow>
  );
}

function JobRow({ job, seen }: { job: JobStatus; seen: FailureKey[] }) {
  const marked = isSeen(seen, { kind: "job", id: job.id });
  return (
    <SelectableRow target={{ kind: "job", id: job.id }} seen={marked}>
      <State {...jobWord(job)} />
      <span className="item-name mono">{job.command}</span>
      <span className="caption mono">{job.id}</span>
      <SeenMark seen={marked} />
    </SelectableRow>
  );
}

function ContainerRow({ container, worktreePath }: { container: ContainerStatus; worktreePath: string }) {
  return (
    <SelectableRow
      target={{ kind: "compose", worktree: worktreePath, service: container.service }}
      seen={false}
    >
      <span className="item-name">{container.service}</span>
      <State word="running" tone="ok" />
      {container.port === null ? null : <span className="caption">{`:${container.port}`}</span>}
    </SelectableRow>
  );
}

function WorktreeView({
  worktree,
  at,
  seen,
  doctor,
}: {
  worktree: WorktreeStatus;
  at: number;
  seen: FailureKey[];
  doctor: DoctorStore;
}) {
  const empty = worktree.services.length === 0 && worktree.jobs.length === 0;
  return (
    <section>
      <h2 className="title">
        <span title={worktreeTooltip(worktree)}>{worktreeLabel(worktree)}</span>{" "}
        <RemovedMark worktree={worktree} />
      </h2>
      {empty ? <p className="muted">Nothing runs in this Worktree.</p> : null}
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
      {(worktree.containers ?? []).length > 0 ? (
        <>
          <h3 className="caption">Compose services</h3>
          <ul className="list">
            {(worktree.containers ?? []).map((container) => (
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

function Inspector({
  current,
  selected,
  at,
  onNeedEditor,
}: {
  current: WorktreeStatus | null;
  selected: Selected | null;
  at: number;
  onNeedEditor: () => void;
}) {
  if (selected !== null) {
    return (
      <>
        <SelectedPanel key={selectedKey(selected)} selected={selected} at={at} />
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
    return <p className="caption">Select a Worktree.</p>;
  }
  return (
    <>
      <WorktreeFacts current={current} />
      <WorktreeActions key={current.path} path={current.path} onNeedEditor={onNeedEditor} />
    </>
  );
}

/** Opens the Worktree in the editor or the terminal the settings name, or reveals it in the Finder. */
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
      <dt className="caption">Path</dt>
      <dd className="mono">{current.path}</dd>
      <dt className="caption">Slug</dt>
      <dd className="mono">{current.slug}</dd>
      <dt className="caption">Kind</dt>
      <dd>{current.main ? "Main worktree" : "Worktree"}</dd>
      <dt className="caption">Services</dt>
      <dd>{current.services.length}</dd>
      <dt className="caption">Jobs</dt>
      <dd>{current.jobs.length}</dd>
    </dl>
  );
}
