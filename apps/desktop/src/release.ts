import { invoke } from "@tauri-apps/api/core";

import { commandIn } from "./services";
import type { ReleaseReport } from "./protocol/ReleaseReport";
import type { Response } from "./protocol/Response";
import type { WorktreeStatus } from "./protocol/WorktreeStatus";

/** The command the sheet stands for, run from the Worktree it releases. */
export function releaseCommand(path: string): string {
  return commandIn(path, "worktree release");
}

function reportOf(response: Response): ReleaseReport {
  switch (response.response) {
    case "worktree_release":
      return response;
    case "error":
      throw new Error(response.message);
    default:
      throw new Error(`unexpected answer from the Daemon: ${response.response}`);
  }
}

/** Asks what releasing the Worktree at `path` would do, with `WorktreeRelease { dry_run: true }`. Nothing changes. */
export async function previewRelease(path: string): Promise<ReleaseReport> {
  return reportOf(await invoke<Response>("preview_release", { path }));
}

/** Releases the Worktree at `path`, with `WorktreeRelease { dry_run: false }`. */
export async function releaseWorktree(path: string): Promise<ReleaseReport> {
  return reportOf(await invoke<Response>("release_worktree", { path }));
}

/**
 * Whether Procly holds nothing in the Worktree: no Service or Job to stop, no Compose service to let go of, no Lease to
 * release, and no Retention policy to apply, since one may delete the data of an Isolated container. A Lease or a
 * Logical resource the release keeps is not held, so it does not count. This is the test
 * `ReleaseReport::released_nothing` makes in Rust, written over the same fields because the window only has the JSON.
 */
export function holdsNothing(report: ReleaseReport): boolean {
  return (
    report.services.length === 0 &&
    report.jobs.length === 0 &&
    report.compose_services.length === 0 &&
    report.leases.length === 0 &&
    report.retention.length === 0
  );
}

/** The ended Jobs and Services of a Worktree, which a release forgets. */
export interface Ended {
  jobs: number;
  services: number;
}

/**
 * How many ended Jobs and ended Services the status holds for a Worktree. The dry run lists only what runs, so the
 * sheet counts the rest from the status the window already has, with no request of its own.
 */
export function endedIn(worktree: Pick<WorktreeStatus, "jobs" | "services">): Ended {
  return {
    jobs: worktree.jobs.filter((job) => job.state === "ended").length,
    services: worktree.services.filter((service) => service.state === "ended").length,
  };
}

/** "2 ended Jobs and 1 ended Service", leaving out the kind that has none. Empty when nothing ended is left. */
export function endedWords({ jobs, services }: Ended): string {
  const parts = [
    ...(jobs === 0 ? [] : [`${jobs} ended ${jobs === 1 ? "Job" : "Jobs"}`]),
    ...(services === 0 ? [] : [`${services} ended ${services === 1 ? "Service" : "Services"}`]),
  ];
  return parts.join(" and ");
}

/** What happens to the container of a Compose service the Worktree lets go of: it keeps running for others, or stops. */
export function composeLine(released: ReleaseReport["compose_services"][number]): string {
  const { service, others, grace_ms } = released;
  if (others === 0) {
    return `${service}: its container stops after ${graceOf(grace_ms)} unless a Worktree needs it again`;
  }
  return `${service}: its container keeps running for ${
    others === 1 ? "1 other Worktree" : `${others} other Worktrees`
  }`;
}

function graceOf(ms: number): string {
  const seconds = Math.round(ms / 1000);
  if (seconds >= 3600 && seconds % 3600 === 0) {
    return `${seconds / 3600} h`;
  }
  if (seconds >= 60 && seconds % 60 === 0) {
    return `${seconds / 60} min`;
  }
  return `${seconds} s`;
}
