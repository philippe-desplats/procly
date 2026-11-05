import { invoke } from "@tauri-apps/api/core";

import { basename } from "./labels";
import type { GcReport } from "./protocol/GcReport";
import type { Orphan } from "./protocol/Orphan";
import type { Response } from "./protocol/Response";

/** The command the sheet stands for. It deletes every Orphan it finds, not only the ones the sheet showed. */
export const COLLECT_COMMAND = "procly gc --apply";

/** The Orphans a removed Worktree left behind. The Daemon lists them, the window only groups them. */
export interface OrphanGroup {
  repository: string;
  worktree: string;
  orphans: Orphan[];
}

/** What an Orphan is, and the data its deletion drops. */
export interface OrphanLine {
  what: string;
  drops: string;
}

function reportOf(response: Response): GcReport {
  switch (response.response) {
    case "gc":
      return response;
    case "error":
      throw new Error(response.message);
    default:
      throw new Error(`unexpected answer from the Daemon: ${response.response}`);
  }
}

/** Asks the Daemon which Orphans exist, with `Gc { apply: false }`. Nothing is deleted. */
export async function listOrphans(): Promise<GcReport> {
  return reportOf(await invoke<Response>("list_orphans"));
}

/**
 * Deletes exactly `only`, the Orphans of the dry run the developer confirmed, sent back as returned. The Daemon deletes
 * those that still are Orphans and reports the others as not deleted. There is no way to ask for every Orphan.
 */
export async function deleteOrphans(only: Orphan[]): Promise<GcReport> {
  return reportOf(await invoke<Response>("delete_orphans", { only }));
}

/** The Orphans grouped by the removed Worktree they belong to, in the order the Daemon listed them. */
export function groupByWorktree(orphans: Orphan[]): OrphanGroup[] {
  const groups = new Map<string, OrphanGroup>();
  for (const orphan of orphans) {
    const key = `${orphan.repository}\n${orphan.worktree}`;
    const group = groups.get(key) ?? {
      repository: orphan.repository,
      worktree: orphan.worktree,
      orphans: [],
    };
    group.orphans.push(orphan);
    groups.set(key, group);
  }
  return [...groups.values()];
}

/** The name of a removed Worktree: git no longer lists it, so only its folder is left to name it. */
export function removedWorktreeLabel(path: string): string {
  return basename(path);
}

export function describeOrphan(orphan: Orphan): OrphanLine {
  switch (orphan.kind) {
    case "lease":
      return {
        what: `Lease ${orphan.service}.${orphan.port_name}, port ${orphan.port}`,
        drops: `Returns port ${orphan.port} to the Range, and removes its Portless alias if it has one.`,
      };
    case "logical_resource":
      return {
        what: `Logical resource ${orphan.service}.${orphan.resource}`,
        drops: `Drops ${orphan.name} in its ${orphan.driver} container, with the data in it.`,
      };
    case "output_directory":
      return {
        what: "Output directory",
        drops: `Deletes ${orphan.path}, the output files of its Services.`,
      };
    case "setup_directory":
      return {
        what: "Setup directory",
        drops: `Deletes ${orphan.path}, the log of a setup command that failed.`,
      };
    case "isolated_container":
      return {
        what: `Isolated container ${orphan.service}`,
        drops: `Removes the Isolated container of ${orphan.service} (compose project ${orphan.project}, service ${orphan.compose}) and its anonymous volumes.`,
      };
    case "isolated_volume":
      return {
        what: `Volume ${orphan.volume}`,
        drops: `Removes the volume ${orphan.volume} of compose project ${orphan.project}, with the data in it.`,
      };
    default: {
      const unhandled: never = orphan;
      throw new Error(`unhandled Orphan: ${JSON.stringify(unhandled)}`);
    }
  }
}
