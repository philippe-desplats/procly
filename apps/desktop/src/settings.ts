import { invoke } from "@tauri-apps/api/core";

/** What the developer chose. An app is the name `open -a` takes, and the editor is null until one is chosen. */
export interface Settings {
  editor: string | null;
  terminal: string;
  /** Whether the app shows a Dock icon while its window is open. */
  show_in_dock: boolean;
  /** Whether a failure that enters "Needs you" makes a banner. Rust changes it through `setNotifyOnFailure` only. */
  notify_on_failure: boolean;
}

/** What `saveSettings` sends: the notification switch has its own command, which asks for the permission first. */
export type SavedSettings = Omit<Settings, "notify_on_failure">;

/** The editors and terminals installed on this Mac, among the ones the app knows. */
export interface DetectedApps {
  editors: string[];
  terminals: string[];
}

/** What a Worktree is opened with. */
export type OpenWith = "editor" | "terminal";

/** Why a Worktree did not open. `no_editor` sends the developer to Settings, every other kind is shown as it is. */
export interface OpenFailure {
  kind: "no_editor" | "missing" | "launch" | "reveal" | "task" | "settings" | "login_item" | "notifications";
  message: string;
}

/** The event the native menu sends when Settings is chosen, `⌘,` included. */
export const OPEN_SETTINGS = "open-settings";

/**
 * The event Rust sends when a banner or a line of "Needs you" in the menu was clicked. Its payload is the `Place` of
 * the item it was about, or null for a banner about several failures.
 */
export const OPEN_FAILURE = "open-failure";

/**
 * What a click opens the window on: a failed Service or Job, selected in the inspector, or the Worktree of a Foreign
 * process. A Job has no Worktree when it started outside any Repository.
 */
export type Place =
  | { kind: "service"; worktree: string; service: string }
  | { kind: "job"; worktree: string | null; id: string }
  | { kind: "worktree"; worktree: string };

export const loadSettings = (): Promise<Settings> => invoke("get_settings");

/** Applies the settings at once. Rust writes them to `settings.json`. */
export const saveSettings = ({ editor, terminal, show_in_dock }: Settings): Promise<void> => {
  const settings: SavedSettings = { editor, terminal, show_in_dock };
  return invoke("save_settings", { settings });
};

/**
 * Asks Rust to turn "Notify on failure" on or off, and answers whether it is on. Turning it on asks for the permission,
 * and a refusal answers false.
 */
export const setNotifyOnFailure = (enabled: boolean): Promise<boolean> =>
  invoke("set_notify_on_failure", { enabled });

/** Asks Rust to open the notification settings of macOS, where the banners of the app are allowed. */
export const openNotificationSettings = (): Promise<void> => invoke("open_notification_settings");

/** Whether the system starts the app at login. It is read from the system every time, the app never stores it. */
export const loadOpenAtLogin = (): Promise<boolean> => invoke("get_open_at_login");

/** Asks Rust to switch Open at login, and answers what the system says afterwards. */
export const saveOpenAtLogin = (enabled: boolean): Promise<boolean> =>
  invoke("set_open_at_login", { enabled });

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
