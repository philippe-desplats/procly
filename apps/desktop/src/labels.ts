import type { WorktreeStatus } from "./protocol/WorktreeStatus";

/** The last segment of a path, which names a Worktree that has no branch to name it. */
export function basename(path: string): string {
  const segments = path.split("/").filter((segment) => segment !== "");
  return segments.at(-1) ?? path;
}

type Named = Pick<WorktreeStatus, "path" | "slug" | "branch">;

/** A Worktree is named by its branch, or by the basename of its path when it has none. Nothing marks a detached `HEAD`. */
export function worktreeLabel(worktree: Named): string {
  const branch = worktree.branch ?? "";
  return branch === "" ? basename(worktree.path) : branch;
}

/** What the tooltip of a Worktree shows: the 3 names it goes by. */
export function worktreeTooltip(worktree: Named): string {
  const branch = worktree.branch ?? "";
  return [
    `Branch: ${branch === "" ? "none" : branch}`,
    `Folder: ${basename(worktree.path)}`,
    `Slug: ${worktree.slug}`,
  ].join("\n");
}

/** A Repository is named after the directory that holds its `.git`. */
export function repositoryLabel(gitCommonDir: string): string {
  const segments = gitCommonDir.split("/").filter((segment) => segment !== "");
  return segments.at(-1) === ".git" ? (segments.at(-2) ?? gitCommonDir) : (segments.at(-1) ?? gitCommonDir);
}
