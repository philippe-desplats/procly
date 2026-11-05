import type { Worktree } from "./protocol/Worktree";

/** The last segment of a path, which names a Worktree until its branch is in the protocol. */
export function basename(path: string): string {
  const segments = path.split("/").filter((segment) => segment !== "");
  return segments.at(-1) ?? path;
}

export function worktreeLabel(worktree: Pick<Worktree, "path">): string {
  return basename(worktree.path);
}

/** A Repository is named after the directory that holds its `.git`. */
export function repositoryLabel(gitCommonDir: string): string {
  const segments = gitCommonDir.split("/").filter((segment) => segment !== "");
  return segments.at(-1) === ".git" ? (segments.at(-2) ?? gitCommonDir) : (segments.at(-1) ?? gitCommonDir);
}
