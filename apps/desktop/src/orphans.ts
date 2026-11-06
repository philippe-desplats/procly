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
  /** Whether what it drops is data lost for good, which the window shows in red. */
  loss: boolean;
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

/** What deleting `count` Orphans of a kind does, in one line that holds the count. The order is the one of the sheet. */
const EFFECTS: Record<Orphan["kind"], (count: number) => string> = {
  lease: (count) =>
    count === 1
      ? "1 Lease: its port goes back to the Range, with its Portless alias."
      : `${count} Leases: their ports go back to the Range, with their Portless aliases.`,
  logical_resource: (count) =>
    count === 1
      ? "1 Logical resource: dropped in its container, with the data in it."
      : `${count} Logical resources: dropped in their containers, with the data in them.`,
  output_directory: (count) =>
    count === 1
      ? "1 output directory: deleted, with the output files of its Services."
      : `${count} output directories: deleted, with the output files of their Services.`,
  setup_directory: (count) =>
    count === 1
      ? "1 setup directory: deleted, with the log of a setup command that failed."
      : `${count} setup directories: deleted, with the logs of setup commands that failed.`,
  isolated_container: (count) =>
    count === 1
      ? "1 Isolated container: removed, with its anonymous volumes."
      : `${count} Isolated containers: removed, with their anonymous volumes.`,
  isolated_volume: (count) =>
    count === 1
      ? "1 volume: removed, with the data in it."
      : `${count} volumes: removed, with the data in them.`,
};

/**
 * Whether deleting an Orphan of a kind loses data that nothing regenerates: a database, a volume and the anonymous
 * volumes of a container. A Lease only gives a port back, and the output and setup directories hold logs and files a
 * Service writes again.
 */
const DROPS_DATA: Record<Orphan["kind"], boolean> = {
  lease: false,
  logical_resource: true,
  output_directory: false,
  setup_directory: false,
  isolated_container: true,
  isolated_volume: true,
};

/** One line of what a collection deletes, and whether the data it drops is lost for good. */
export interface OrphanEffect {
  text: string;
  loss: boolean;
}

/** What a collection of `orphans` deletes: one line per kind it holds, with how many and the data they drop. */
export function collectionEffects(orphans: Orphan[]): OrphanEffect[] {
  const counts = new Map<Orphan["kind"], number>();
  for (const orphan of orphans) {
    counts.set(orphan.kind, (counts.get(orphan.kind) ?? 0) + 1);
  }
  return (Object.keys(EFFECTS) as Orphan["kind"][]).flatMap((kind) => {
    const count = counts.get(kind) ?? 0;
    return count === 0 ? [] : [{ text: EFFECTS[kind](count), loss: DROPS_DATA[kind] }];
  });
}

export function describeOrphan(orphan: Orphan): OrphanLine {
  const line = describeKind(orphan);
  return { ...line, loss: DROPS_DATA[orphan.kind] };
}

function describeKind(orphan: Orphan): Pick<OrphanLine, "what" | "drops"> {
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
