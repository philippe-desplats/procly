import type { ReactNode } from "react";

import type { FailureKey } from "./daemon";
import { isSeen } from "./needs-you";
import type { JobStatus } from "./protocol/JobStatus";
import type { ServiceStatus } from "./protocol/ServiceStatus";
import type { WorktreeStatus } from "./protocol/WorktreeStatus";
import { jobWord, serviceWord, type StateWord } from "./state";
import { formatMoment } from "./time";

export function State({ word, tone }: StateWord) {
  return (
    <span className={`state tone-${tone}`}>
      <span className="dot" aria-hidden="true" />
      {word}
    </span>
  );
}

/** Git no longer lists the Worktree, though the Daemon still runs something in it. */
export function RemovedMark({ worktree }: { worktree: WorktreeStatus }) {
  return worktree.listed ? null : <span className="mark">Removed</span>;
}

/** The mark of a failure the developer saw: its row stays, dimmed, and says so in words as well. */
function SeenMark({ seen }: { seen: boolean }) {
  return seen ? <span className="mark">Seen</span> : null;
}

/** A Service with its state, port and start time. `controls` sit under it, on the screen of one Worktree. */
export function ServiceRow({
  service,
  worktreePath,
  at,
  seen,
  controls,
}: {
  service: ServiceStatus;
  worktreePath: string;
  at: number;
  seen: FailureKey[];
  controls?: ReactNode;
}) {
  const marked = isSeen(seen, {
    kind: "service",
    worktree: worktreePath,
    service: service.service,
    started_at: service.started_at,
  });
  const line = (
    <>
      <span className="item-name">{service.service}</span>
      <State {...serviceWord(service)} />
      {service.port === null ? null : <span className="caption">{`:${service.port}`}</span>}
      <span className="caption">{formatMoment(service.started_at, at)}</span>
      <SeenMark seen={marked} />
    </>
  );
  return controls === undefined ? (
    <li className="item" data-seen={marked ? "true" : undefined}>
      {line}
    </li>
  ) : (
    <li className="item item-block" data-seen={marked ? "true" : undefined}>
      <div className="item-line">{line}</div>
      {controls}
    </li>
  );
}

/** A Job with its state and id. `where` names the directory it started in, for the Jobs that belong to no Worktree. */
export function JobRow({ job, seen, where }: { job: JobStatus; seen: FailureKey[]; where?: string }) {
  const marked = isSeen(seen, { kind: "job", id: job.id });
  return (
    <li className="item" data-seen={marked ? "true" : undefined}>
      <State {...jobWord(job)} />
      <span className="item-name mono">{job.command}</span>
      <span className="caption mono">{job.id}</span>
      {where === undefined ? null : <span className="caption mono">{where}</span>}
      <SeenMark seen={marked} />
    </li>
  );
}
