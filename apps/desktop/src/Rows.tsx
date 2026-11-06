import type { FailureKey } from "./daemon";
import { isSeen, useFailed } from "./needs-you";
import type { ContainerStatus } from "./protocol/ContainerStatus";
import type { JobStatus } from "./protocol/JobStatus";
import type { ServiceStatus } from "./protocol/ServiceStatus";
import type { WorktreeStatus } from "./protocol/WorktreeStatus";
import { SelectableRow } from "./selectable";
import { jobWord, serviceWord } from "./state";
import { State } from "./StateMark";
import { formatMoment } from "./time";

/** Git no longer lists the Worktree, though the Daemon still runs something in it. */
export function RemovedMark({ worktree }: { worktree: WorktreeStatus }) {
  return worktree.listed ? null : <span className="mark">Removed</span>;
}

/** The mark of a failure the developer saw: its row stays, dimmed, and says so in words as well. */
export function SeenMark({ seen }: { seen: boolean }) {
  return seen ? <span className="mark">Seen</span> : null;
}

/** A Service with its state, port and start time. The inspector explains it and holds its Stop and Restart. */
export function ServiceRow({
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
  const key: FailureKey = {
    kind: "service",
    worktree: worktreePath,
    service: service.service,
    started_at: service.started_at,
  };
  const marked = isSeen(seen, key);
  const failed = useFailed(key);
  return (
    <SelectableRow
      target={{ kind: "service", worktree: worktreePath, service: service.service }}
      seen={marked}
    >
      <span className="item-name">{service.service}</span>
      <State {...serviceWord(service, failed)} />
      {service.port === null ? null : <span className="caption">{`:${service.port}`}</span>}
      <span className="caption">{formatMoment(service.started_at, at)}</span>
      <SeenMark seen={marked} />
    </SelectableRow>
  );
}

export function JobRow({ job, seen }: { job: JobStatus; seen: FailureKey[] }) {
  const key: FailureKey = { kind: "job", id: job.id };
  const marked = isSeen(seen, key);
  const failed = useFailed(key);
  return (
    <SelectableRow target={{ kind: "job", id: job.id }} seen={marked}>
      <State {...jobWord(job, failed)} />
      <span className="item-name mono">{job.command}</span>
      <span className="caption mono">{job.id}</span>
      <SeenMark seen={marked} />
    </SelectableRow>
  );
}

/** An Isolated Compose service of a Worktree. The inspector holds its Stop. */
export function ContainerRow({
  container,
  worktreePath,
}: {
  container: ContainerStatus;
  worktreePath: string;
}) {
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
