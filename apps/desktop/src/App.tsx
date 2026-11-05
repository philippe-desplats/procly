import { useEffect, useState, useSyncExternalStore } from "react";

import type { DaemonSnapshot, DaemonStore } from "./daemon";
import { repositoryLabel, worktreeLabel } from "./labels";
import type { RepositoryStatus } from "./protocol/RepositoryStatus";
import type { WorktreeStatus } from "./protocol/WorktreeStatus";
import { jobWord, serviceWord, type StateWord } from "./state";

export function App({ store }: { store: DaemonStore }) {
  const snapshot = useSyncExternalStore(store.subscribe, store.getSnapshot);
  const [selectedPath, select] = useState<string | null>(null);

  useEffect(() => {
    void store.refresh();
  }, [store]);

  const repositories = snapshot.kind === "status" ? snapshot.report.repositories : [];
  const worktrees = repositories.flatMap((repository) => repository.worktrees);
  const current = worktrees.find((worktree) => worktree.path === selectedPath) ?? worktrees[0] ?? null;

  return (
    <div className="window">
      <nav className="pane sidebar" aria-label="Worktrees">
        <h1 className="pane-title">Procly</h1>
        <Sidebar snapshot={snapshot} repositories={repositories} current={current} onSelect={select} />
      </nav>
      <main className="pane center">
        <Center snapshot={snapshot} current={current} />
      </main>
      <aside className="pane inspector" aria-label="Inspector">
        <Inspector current={current} />
      </aside>
    </div>
  );
}

function Sidebar({
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
                  aria-current={worktree.path === current?.path ? "true" : undefined}
                  onClick={() => {
                    onSelect(worktree.path);
                  }}
                >
                  <span className="row-name">{worktreeLabel(worktree)}</span>
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

function Center({ snapshot, current }: { snapshot: DaemonSnapshot; current: WorktreeStatus | null }) {
  switch (snapshot.kind) {
    case "loading":
      return <p className="caption">Reading the Daemon…</p>;
    case "no-daemon":
      return (
        <section>
          <h2 className="title">Daemon not running</h2>
          <p className="muted">No Daemon answers on its socket, and this app does not start one.</p>
        </section>
      );
    case "failure":
      return (
        <section>
          <h2 className="title">The Daemon did not answer</h2>
          <p className="muted">{snapshot.message}</p>
        </section>
      );
    case "status":
      return current === null ? (
        <section>
          <h2 className="title">Nothing to show</h2>
          <p className="muted">The Daemon runs nothing yet.</p>
        </section>
      ) : (
        <WorktreeView worktree={current} />
      );
    default: {
      const unhandled: never = snapshot;
      throw new Error(`unhandled snapshot: ${JSON.stringify(unhandled)}`);
    }
  }
}

function WorktreeView({ worktree }: { worktree: WorktreeStatus }) {
  const empty = worktree.services.length === 0 && worktree.jobs.length === 0;
  return (
    <section>
      <h2 className="title">{worktreeLabel(worktree)}</h2>
      {empty ? <p className="muted">Nothing runs in this Worktree.</p> : null}
      {worktree.services.length > 0 ? (
        <>
          <h3 className="caption">Services</h3>
          <ul className="list">
            {worktree.services.map((service) => (
              <li key={service.service} className="item">
                <State {...serviceWord(service)} />
                <span className="item-name">{service.service}</span>
                <span className="caption">{service.port === null ? "" : `:${service.port}`}</span>
              </li>
            ))}
          </ul>
        </>
      ) : null}
      {worktree.jobs.length > 0 ? (
        <>
          <h3 className="caption">Jobs</h3>
          <ul className="list">
            {worktree.jobs.map((job) => (
              <li key={job.id} className="item">
                <State {...jobWord(job)} />
                <span className="item-name mono">{job.command}</span>
                <span className="caption mono">{job.id}</span>
              </li>
            ))}
          </ul>
        </>
      ) : null}
    </section>
  );
}

function State({ word, tone }: StateWord) {
  return (
    <span className={`state tone-${tone}`}>
      <span className="dot" aria-hidden="true" />
      {word}
    </span>
  );
}

function Inspector({ current }: { current: WorktreeStatus | null }) {
  if (current === null) {
    return <p className="caption">Select a Worktree.</p>;
  }
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
