import type { ContainerStatus } from "./protocol/ContainerStatus";
import type { ForeignProcess } from "./protocol/ForeignProcess";
import type { JobStatus } from "./protocol/JobStatus";
import type { ServiceStatus } from "./protocol/ServiceStatus";
import type { StatusReport } from "./protocol/StatusReport";
import type { WorktreeStatus } from "./protocol/WorktreeStatus";
import { repositoryLabel } from "./labels";

/**
 * What the developer picked in the center: the item the inspector explains and follows. It names the item and holds
 * nothing of its state, so that each answer of the poller gives the inspector the item as it is now, and the follow does
 * not start again at every tick.
 */
export type Selection =
  | { kind: "service"; worktree: string; service: string }
  | { kind: "job"; id: string }
  | { kind: "compose"; worktree: string; service: string }
  | { kind: "foreign"; worktree: string; process: string };

/** A selection read in the last answer of the Daemon. */
export type Selected =
  | { kind: "service"; worktree: WorktreeStatus; service: ServiceStatus; repository: string }
  | { kind: "job"; worktree: WorktreeStatus | null; job: JobStatus }
  | { kind: "compose"; worktree: WorktreeStatus; container: ContainerStatus }
  | { kind: "foreign"; worktree: WorktreeStatus; process: ForeignProcess };

/** A selection whose item Procly supervises, so that the inspector follows its output. A Foreign process has none. */
export type Followed = Exclude<Selected, { kind: "foreign" }>;

/**
 * What tells a Foreign process from the others of its Worktree: its pid, or the name of its container. A pid is not
 * kept across a restart of the process, so a process that started again is another one.
 */
export function foreignId(process: ForeignProcess): string {
  return process.kind === "process" ? `pid:${String(process.pid)}` : `container:${process.name}`;
}

/**
 * The Foreign process the Doctor takes the developer to from a failed `leased_ports` check, chosen from the Status
 * alone: the first, in Status order, that holds a port a Service of its own Worktree has, and `null` when none does.
 * The message of the check is text for a person and is never parsed (ADR 0009).
 */
export function foreignToShow(report: StatusReport): Selection | null {
  for (const worktree of report.repositories.flatMap((repository) => repository.worktrees)) {
    const chosen = (worktree.foreign ?? []).find((process) =>
      process.ports.some((port) => worktree.services.some((service) => service.port === port)),
    );
    if (chosen !== undefined) {
      return { kind: "foreign", worktree: worktree.path, process: foreignId(chosen) };
    }
  }
  return null;
}

/** Whether `selection` is the item `other` names. */
export function sameSelection(selection: Selection | null, other: Selection): boolean {
  if (selection === null || selection.kind !== other.kind) {
    return false;
  }
  switch (selection.kind) {
    case "service":
    case "compose":
      return (
        other.kind === selection.kind &&
        selection.worktree === other.worktree &&
        selection.service === other.service
      );
    case "foreign":
      return (
        other.kind === "foreign" &&
        selection.worktree === other.worktree &&
        selection.process === other.process
      );
    case "job":
      return other.kind === "job" && selection.id === other.id;
    default: {
      const unhandled: never = selection;
      throw new Error(`unhandled selection: ${JSON.stringify(unhandled)}`);
    }
  }
}

/**
 * The item `selection` names in `report`, or `null` once the Daemon no longer reports it: an ended Job it evicted, a
 * Worktree it released.
 */
export function resolve(selection: Selection | null, report: StatusReport): Selected | null {
  if (selection === null) {
    return null;
  }
  const worktrees = report.repositories.flatMap((repository) => repository.worktrees);
  switch (selection.kind) {
    case "service": {
      const repository = report.repositories.find((candidate) =>
        candidate.worktrees.some((worktree) => worktree.path === selection.worktree),
      );
      const worktree = repository?.worktrees.find((candidate) => candidate.path === selection.worktree);
      const service = worktree?.services.find((candidate) => candidate.service === selection.service);
      return repository === undefined || worktree === undefined || service === undefined
        ? null
        : { kind: "service", worktree, service, repository: repositoryLabel(repository.git_common_dir) };
    }
    case "compose": {
      const worktree = worktrees.find((candidate) => candidate.path === selection.worktree);
      const container = worktree?.containers?.find((candidate) => candidate.service === selection.service);
      return worktree === undefined || container === undefined
        ? null
        : { kind: "compose", worktree, container };
    }
    case "foreign": {
      const worktree = worktrees.find((candidate) => candidate.path === selection.worktree);
      const process = worktree?.foreign?.find((candidate) => foreignId(candidate) === selection.process);
      return worktree === undefined || process === undefined ? null : { kind: "foreign", worktree, process };
    }
    case "job": {
      for (const worktree of worktrees) {
        const job = worktree.jobs.find((candidate) => candidate.id === selection.id);
        if (job !== undefined) {
          return { kind: "job", worktree, job };
        }
      }
      const job = report.jobs_outside_repositories.find((candidate) => candidate.id === selection.id);
      return job === undefined ? null : { kind: "job", worktree: null, job };
    }
    default: {
      const unhandled: never = selection;
      throw new Error(`unhandled selection: ${JSON.stringify(unhandled)}`);
    }
  }
}

/** What tells one selected item from another, so that the panel of an item starts from nothing when another is picked. */
export function selectedKey(selected: Selected): string {
  switch (selected.kind) {
    case "service":
      return `service\0${selected.worktree.path}\0${selected.service.service}`;
    case "compose":
      return `compose\0${selected.worktree.path}\0${selected.container.service}`;
    case "foreign":
      return `foreign\0${selected.worktree.path}\0${foreignId(selected.process)}`;
    case "job":
      return `job\0${selected.job.id}`;
    default: {
      const unhandled: never = selected;
      throw new Error(`unhandled selection: ${JSON.stringify(unhandled)}`);
    }
  }
}
