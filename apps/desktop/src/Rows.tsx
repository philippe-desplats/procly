import type { FailureKey } from "./daemon";
import { ItemRow } from "./ItemRow";
import { basename, worktreeTooltip } from "./labels";
import { isSeen, useFailed } from "./needs-you";
import type { ContainerStatus } from "./protocol/ContainerStatus";
import type { ForeignProcess } from "./protocol/ForeignProcess";
import type { JobStatus } from "./protocol/JobStatus";
import type { ServiceStatus } from "./protocol/ServiceStatus";
import type { WorktreeStatus } from "./protocol/WorktreeStatus";
import { jobWord, serviceWord } from "./state";
import { formatMoment } from "./time";

/** Git no longer lists the Worktree, though the Daemon still runs something in it. */
export function RemovedMark({ worktree }: { worktree: WorktreeStatus }) {
  return worktree.listed ? null : <span className="mark">Removed</span>;
}

/** What a Foreign process leaves to say after its name, quietly: its pid, or the Compose project and service it is. */
export function foreignDetail(process: ForeignProcess): string {
  return process.kind === "process"
    ? `pid ${String(process.pid)}`
    : `${process.project} / ${process.service}`;
}

/**
 * The Worktree a row runs in, as the props of `ItemRow` that name it: its folder, since the folders of the Worktrees
 * of every Repository tell them apart, and its 3 names in the tooltip.
 */
export function worktreeWhere(worktree: WorktreeStatus) {
  return {
    where: (
      <>
        {basename(worktree.path)}
        <RemovedMark worktree={worktree} />
      </>
    ),
    whereTitle: worktreeTooltip(worktree),
  };
}

/**
 * A Service with its state, port and start time. The inspector explains it and holds its Stop and Restart. `worktree`
 * is given where the row names the Worktree it runs in, the Overview.
 */
export function ServiceRow({
  service,
  worktreePath,
  worktree,
  at,
  seen,
}: {
  service: ServiceStatus;
  worktreePath: string;
  worktree?: WorktreeStatus;
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
    <ItemRow
      target={{ kind: "service", worktree: worktreePath, service: service.service }}
      seen={marked}
      state={serviceWord(service, failed)}
      name={service.service}
      {...(worktree === undefined ? {} : worktreeWhere(worktree))}
      moment={formatMoment(service.started_at, at)}
      ports={service.port === null ? [] : [service.port]}
    />
  );
}

export function JobRow({ job, seen }: { job: JobStatus; seen: FailureKey[] }) {
  const key: FailureKey = { kind: "job", id: job.id };
  const marked = isSeen(seen, key);
  const failed = useFailed(key);
  return (
    <ItemRow
      target={{ kind: "job", id: job.id }}
      seen={marked}
      state={jobWord(job, failed)}
      name={job.command}
      mono
    >
      <span className="caption mono">{job.id}</span>
    </ItemRow>
  );
}

/**
 * An Isolated Compose service of a Worktree, listed with the Services and said to be Isolated beside its name, since
 * the kind is said in text. The inspector holds its Stop.
 */
export function ContainerRow({
  container,
  worktreePath,
  worktree,
}: {
  container: ContainerStatus;
  worktreePath: string;
  worktree?: WorktreeStatus;
}) {
  return (
    <ItemRow
      target={{ kind: "compose", worktree: worktreePath, service: container.service }}
      seen={false}
      state={{ word: "running", tone: "ok", glyph: "healthy" }}
      name={container.service}
      detail="Isolated"
      {...(worktree === undefined ? {} : worktreeWhere(worktree))}
      ports={container.port === null ? [] : [container.port]}
    />
  );
}
