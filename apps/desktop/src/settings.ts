import { invoke } from "@tauri-apps/api/core";

/** What the developer chose. An app is the name `open -a` takes, and the editor is null until one is chosen. */
export interface Settings {
  editor: string | null;
  terminal: string;
}

/** The editors and terminals installed on this Mac, among the ones the app knows. */
export interface DetectedApps {
  editors: string[];
  terminals: string[];
}

/** What a Worktree is opened with. */
export type OpenWith = "editor" | "terminal";

/** Why a Worktree did not open. `no_editor` sends the developer to Settings, every other kind is shown as it is. */
export interface OpenFailure {
  kind: "no_editor" | "missing" | "launch" | "reveal" | "task";
  message: string;
}

/** The event the native menu sends when Settings is chosen, `⌘,` included. */
export const OPEN_SETTINGS = "open-settings";

export const loadSettings = (): Promise<Settings> => invoke("get_settings");

/** Applies the settings at once. Rust writes them to `settings.json`. */
export const saveSettings = (settings: Settings): Promise<void> => invoke("save_settings", { settings });

export const detectedApps = (): Promise<DetectedApps> => invoke("detected_apps");

/** Asks Rust to open the Worktree at `path`, with the app the settings name for `with`. */
export const openWorktree = (path: string, withApp: OpenWith): Promise<void> =>
  invoke("open_worktree", { path, with: withApp });

/** Asks Rust to show the Worktree at `path` in the Finder. */
export const revealWorktree = (path: string): Promise<void> => invoke("reveal_worktree", { path });

/** The error of a command as Rust sent it: an object with a kind and a message, or whatever else rejected. */
export function failureOf(error: unknown): OpenFailure {
  if (typeof error === "object" && error !== null && "kind" in error && "message" in error) {
    return error as OpenFailure;
  }
  return { kind: "task", message: error instanceof Error ? error.message : String(error) };
}
