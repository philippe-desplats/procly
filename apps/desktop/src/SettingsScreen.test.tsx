import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { emit } from "@tauri-apps/api/event";
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { afterEach, describe, expect, it } from "vitest";

import { App } from "./App";
import { createDaemonStore, type Update } from "./daemon";
import recorded from "./fixtures/status.json";
import type { Response } from "./protocol/Response";
import type { DetectedApps, Settings } from "./settings";

/** A `Status { all: true }` answer recorded from a running Daemon, its paths replaced by neutral ones. */
const status: Response = recorded as Response;

const AT = Date.UTC(2026, 10, 5, 19, 35, 0);

const FIXED: Update = { at_ms: AT, response: status, needs_you: [], seen: [] };

const APPS: DetectedApps = {
  editors: ["Visual Studio Code", "Cursor", "Zed"],
  terminals: ["Terminal", "iTerm", "Ghostty"],
};

interface Call {
  command: string;
  args: unknown;
}

/**
 * Plays the Rust side. `settings` is what `settings.json` holds, `opening` decides what an attempt to open answers,
 * and every command the window sends is kept in order.
 */
function app(
  options: {
    /** What `settings.json` holds. Notifications are off unless it says otherwise. */
    settings?: Omit<Settings, "notify_on_failure"> & { notify_on_failure?: boolean };
    apps?: DetectedApps;
    opening?: (command: string, args: unknown) => unknown;
    saving?: () => unknown;
    /** What the system says about Open at login, and what it answers when the switch is asked to change. */
    login?: { enabled: boolean; switching?: (enabled: boolean) => unknown };
    /** What the system answers when the notification permission is asked, and what opening its settings does. */
    notifications?: { permission?: "granted" | "refused"; failure?: unknown; pane?: () => unknown };
  } = {},
): Call[] {
  const calls: Call[] = [];
  let loginItem = options.login?.enabled ?? false;
  let notifying = options.settings?.notify_on_failure ?? false;
  mockIPC(
    (command, args) => {
      calls.push({ command, args });
      switch (command) {
        case "last_update":
          return FIXED;
        case "get_settings":
          return {
            ...(options.settings ?? { editor: null, terminal: "Terminal", show_in_dock: true }),
            notify_on_failure: notifying,
          };
        case "set_notify_on_failure": {
          if (options.notifications?.failure !== undefined) {
            return Promise.reject(options.notifications.failure);
          }
          const { enabled } = args as { enabled: boolean };
          notifying = enabled && options.notifications?.permission !== "refused";
          return notifying;
        }
        case "open_notification_settings":
          return options.notifications?.pane?.();
        case "detected_apps":
          return options.apps ?? APPS;
        case "get_open_at_login":
          return loginItem;
        case "set_open_at_login": {
          const { enabled } = args as { enabled: boolean };
          const refused = options.login?.switching?.(enabled);
          if (refused !== undefined) {
            return refused;
          }
          loginItem = enabled;
          return loginItem;
        }
        case "save_settings":
          return options.saving?.();
        case "doctor":
          // The Worktree screen asks for its checks when it opens, which these tests do not look at.
          return { schema_version: 1, result: "healthy", checks: [], prompt: null, agent: null };
        case "open_worktree":
        case "reveal_worktree":
          return options.opening?.(command, args);
        default:
          return Promise.reject(new Error(`unexpected command ${command}`));
      }
    },
    { shouldMockEvents: true },
  );
  return calls;
}

function saved(calls: Call[]): unknown[] {
  return calls.filter((call) => call.command === "save_settings").map((call) => call.args);
}

async function openSettings(): Promise<void> {
  fireEvent.click(await screen.findByRole("button", { name: /^Settings/ }));
  await screen.findByRole("heading", { name: "Open with" });
}

describe("the Settings screen", () => {
  afterEach(() => {
    cleanup();
    clearMocks();
  });

  it("is an entry pinned at the bottom of the sidebar, and replaces the inspector with the screen", async () => {
    app();
    render(<App store={createDaemonStore()} />);

    const sidebar = await screen.findByRole("navigation", { name: "Worktrees" });
    const entries = within(sidebar).getAllByRole("button");
    expect(entries.at(-1)).toHaveAccessibleName(/^Settings/);

    fireEvent.click(entries.at(-1)!);

    expect(await screen.findByRole("heading", { name: "Settings" })).toBeInTheDocument();
    expect(screen.queryByRole("complementary", { name: "Inspector" })).not.toBeInTheDocument();
    expect(entries.at(-1)).toHaveAttribute("aria-current", "true");
    expect(within(sidebar).getByRole("button", { name: /^Overview/ })).not.toHaveAttribute("aria-current");
  });

  it("shows the editor and the terminal the Rust side holds, among the apps it detected", async () => {
    app({ settings: { editor: "Cursor", terminal: "iTerm", show_in_dock: true } });
    render(<App store={createDaemonStore()} />);

    await openSettings();

    const editor = await screen.findByRole("combobox", { name: "Editor" });
    await waitFor(() => {
      expect(editor).toHaveValue("Cursor");
    });
    expect(
      within(editor)
        .getAllByRole("option")
        .map((option) => option.textContent),
    ).toEqual(["None", "Visual Studio Code", "Cursor", "Zed", "Other…"]);
    expect(screen.getByRole("combobox", { name: "Terminal" })).toHaveValue("iTerm");
  });

  it("starts with Terminal and no editor", async () => {
    app();
    render(<App store={createDaemonStore()} />);

    await openSettings();

    await waitFor(() => {
      expect(screen.getByRole("combobox", { name: "Terminal" })).toHaveValue("Terminal");
    });
    expect(screen.getByRole("combobox", { name: "Editor" })).toHaveValue("");
  });

  it("applies a choice at once, with no Save button, and keeps the other field", async () => {
    const calls = app({ settings: { editor: null, terminal: "iTerm", show_in_dock: true } });
    render(<App store={createDaemonStore()} />);
    await openSettings();
    const editor = await screen.findByRole("combobox", { name: "Editor" });
    await waitFor(() => {
      expect(screen.getByRole("combobox", { name: "Terminal" })).toHaveValue("iTerm");
    });

    fireEvent.change(editor, { target: { value: "Zed" } });

    await waitFor(() => {
      expect(saved(calls)).toEqual([{ settings: { editor: "Zed", terminal: "iTerm", show_in_dock: true } }]);
    });
    expect(screen.queryByRole("button", { name: /save/i })).not.toBeInTheDocument();

    fireEvent.change(screen.getByRole("combobox", { name: "Terminal" }), { target: { value: "Ghostty" } });

    await waitFor(() => {
      expect(saved(calls).at(-1)).toEqual({
        settings: { editor: "Zed", terminal: "Ghostty", show_in_dock: true },
      });
    });
  });

  it("takes None as no editor", async () => {
    const calls = app({ settings: { editor: "Zed", terminal: "Terminal", show_in_dock: true } });
    render(<App store={createDaemonStore()} />);
    await openSettings();
    const editor = await screen.findByRole("combobox", { name: "Editor" });
    await waitFor(() => {
      expect(editor).toHaveValue("Zed");
    });

    fireEvent.change(editor, { target: { value: "" } });

    await waitFor(() => {
      expect(saved(calls)).toEqual([
        { settings: { editor: null, terminal: "Terminal", show_in_dock: true } },
      ]);
    });
  });

  it("takes any other app by its name, applied as it is typed", async () => {
    const calls = app();
    render(<App store={createDaemonStore()} />);
    await openSettings();
    const editor = await screen.findByRole("combobox", { name: "Editor" });
    await waitFor(() => {
      expect(screen.getByRole("combobox", { name: "Terminal" })).toHaveValue("Terminal");
    });

    fireEvent.change(editor, { target: { value: "__other__" } });
    const name = await screen.findByRole("textbox", { name: "Editor app name" });
    fireEvent.change(name, { target: { value: "Nova" } });

    await waitFor(() => {
      expect(saved(calls)).toEqual([
        { settings: { editor: "Nova", terminal: "Terminal", show_in_dock: true } },
      ]);
    });
  });

  it("shows an app that was typed and is not detected as Other, with its name", async () => {
    app({ settings: { editor: "Nova", terminal: "Terminal", show_in_dock: true } });
    render(<App store={createDaemonStore()} />);

    await openSettings();

    expect(await screen.findByRole("textbox", { name: "Editor app name" })).toHaveValue("Nova");
    expect(screen.getByRole("combobox", { name: "Editor" })).toHaveValue("__other__");
  });

  it("keeps the default terminal when the name typed is cleared", async () => {
    const calls = app({ settings: { editor: null, terminal: "Alacritty", show_in_dock: true } });
    render(<App store={createDaemonStore()} />);
    await openSettings();

    const name = await screen.findByRole("textbox", { name: "Terminal app name" });
    fireEvent.change(name, { target: { value: "" } });

    await act(async () => {
      await Promise.resolve();
    });
    expect(saved(calls)).toEqual([]);
  });

  it("is free text where no app is detected", async () => {
    app({ apps: { editors: [], terminals: [] } });
    render(<App store={createDaemonStore()} />);

    await openSettings();

    expect(await screen.findByRole("textbox", { name: "Editor app name" })).toBeInTheDocument();
    expect(screen.getByRole("textbox", { name: "Terminal app name" })).toHaveValue("Terminal");
    expect(screen.queryByRole("combobox")).not.toBeInTheDocument();
    expect(screen.getByText(/not guaranteed/)).toBeInTheDocument();
  });

  it("says when a change could not be saved, and keeps what was chosen", async () => {
    app({
      saving: () =>
        Promise.reject({ kind: "settings", message: "cannot save the settings to /p/cfg: denied" }),
    });
    render(<App store={createDaemonStore()} />);
    await openSettings();
    const editor = await screen.findByRole("combobox", { name: "Editor" });
    await waitFor(() => {
      expect(screen.getByRole("combobox", { name: "Terminal" })).toHaveValue("Terminal");
    });

    fireEvent.change(editor, { target: { value: "Zed" } });

    expect(await screen.findByRole("alert")).toHaveTextContent("cannot save the settings to /p/cfg: denied");
    expect(editor).toHaveValue("Zed");
  });

  it("opens when the native menu sends Settings, and ⌘, comes from there", async () => {
    app();
    render(<App store={createDaemonStore()} />);
    await screen.findByRole("button", { name: "web-fix" });

    await act(async () => {
      await emit("open-settings");
    });

    expect(await screen.findByRole("heading", { name: "Open with" })).toBeInTheDocument();
  });
});

describe("the General section of Settings", () => {
  afterEach(() => {
    cleanup();
    clearMocks();
  });

  it("holds the Dock switch, on by default, and Open at login, off by default", async () => {
    app();
    render(<App store={createDaemonStore()} />);

    await openSettings();

    expect(await screen.findByRole("heading", { name: "General" })).toBeInTheDocument();
    expect(screen.getByRole("switch", { name: "Show in Dock while the window is open" })).toBeChecked();
    expect(screen.getByRole("switch", { name: "Open at login" })).not.toBeChecked();
  });

  it("applies the Dock switch at once and keeps the other settings", async () => {
    const calls = app({ settings: { editor: "Zed", terminal: "iTerm", show_in_dock: true } });
    render(<App store={createDaemonStore()} />);
    await openSettings();
    const dock = await screen.findByRole("switch", { name: "Show in Dock while the window is open" });

    fireEvent.click(dock);

    await waitFor(() => {
      expect(saved(calls)).toEqual([{ settings: { editor: "Zed", terminal: "iTerm", show_in_dock: false } }]);
    });
    expect(dock).not.toBeChecked();
  });

  it("shows the Dock switch off when the settings say so", async () => {
    app({ settings: { editor: null, terminal: "Terminal", show_in_dock: false } });
    render(<App store={createDaemonStore()} />);

    await openSettings();

    expect(
      await screen.findByRole("switch", { name: "Show in Dock while the window is open" }),
    ).not.toBeChecked();
  });

  it("reads Open at login from the system, so that a login item removed elsewhere shows as off", async () => {
    app({ login: { enabled: true } });
    render(<App store={createDaemonStore()} />);
    await openSettings();
    await waitFor(() => {
      expect(screen.getByRole("switch", { name: "Open at login" })).toBeChecked();
    });
    cleanup();
    clearMocks();

    app({ login: { enabled: false } });
    render(<App store={createDaemonStore()} />);
    await openSettings();

    await waitFor(() => {
      expect(screen.getByRole("switch", { name: "Open at login" })).not.toBeChecked();
    });
  });

  it("asks Rust to switch Open at login and shows what the system says afterwards", async () => {
    const calls = app();
    render(<App store={createDaemonStore()} />);
    await openSettings();
    const login = await screen.findByRole("switch", { name: "Open at login" });

    fireEvent.click(login);

    await waitFor(() => {
      expect(login).toBeChecked();
    });
    expect(calls.filter((call) => call.command === "set_open_at_login")).toEqual([
      { command: "set_open_at_login", args: { enabled: true } },
    ]);
  });

  it("never sends Open at login with the settings, which Rust does not store", async () => {
    const calls = app();
    render(<App store={createDaemonStore()} />);
    await openSettings();
    fireEvent.click(await screen.findByRole("switch", { name: "Open at login" }));
    fireEvent.click(screen.getByRole("switch", { name: "Show in Dock while the window is open" }));

    await waitFor(() => {
      expect(saved(calls)).toHaveLength(1);
    });
    expect(JSON.stringify(saved(calls))).not.toContain("login");
  });

  it("says when the system refuses the change and shows the state it kept", async () => {
    app({
      login: {
        enabled: false,
        switching: () =>
          Promise.reject({ kind: "login_item", message: "cannot change Open at login: denied" }),
      },
    });
    render(<App store={createDaemonStore()} />);
    await openSettings();
    const login = await screen.findByRole("switch", { name: "Open at login" });

    fireEvent.click(login);

    expect(await screen.findByRole("alert")).toHaveTextContent("cannot change Open at login: denied");
    await waitFor(() => {
      expect(login).not.toBeChecked();
    });
  });
});

function notificationCalls(calls: Call[]): Call[] {
  return calls.filter((call) => call.command === "set_notify_on_failure");
}

describe("the Notifications section of Settings", () => {
  afterEach(() => {
    cleanup();
    clearMocks();
  });

  it("holds a Notify on failure switch, off by default", async () => {
    app();
    render(<App store={createDaemonStore()} />);

    await openSettings();

    expect(await screen.findByRole("heading", { name: "Notifications" })).toBeInTheDocument();
    expect(await screen.findByRole("switch", { name: "Notify on failure" })).not.toBeChecked();
  });

  it("asks for no permission by opening Settings, only when the switch is turned on", async () => {
    const calls = app();
    render(<App store={createDaemonStore()} />);
    await openSettings();
    const notify = await screen.findByRole("switch", { name: "Notify on failure" });
    await act(async () => {
      await Promise.resolve();
    });

    expect(notificationCalls(calls)).toEqual([]);

    fireEvent.click(notify);

    await waitFor(() => {
      expect(notify).toBeChecked();
    });
    expect(notificationCalls(calls)).toEqual([{ command: "set_notify_on_failure", args: { enabled: true } }]);
  });

  it("states that macOS decides the banner and points to System Settings > Notifications", async () => {
    app();
    render(<App store={createDaemonStore()} />);

    await openSettings();

    const section = await screen.findByRole("region", { name: "Notifications" });
    expect(section).toHaveTextContent(/macOS decides whether the banner shows/);
    expect(section).toHaveTextContent("System Settings > Notifications");
  });

  it("links to the notification settings of the system", async () => {
    const calls = app();
    render(<App store={createDaemonStore()} />);
    await openSettings();

    fireEvent.click(await screen.findByRole("button", { name: "Open notification settings" }));

    await waitFor(() => {
      expect(calls.filter((call) => call.command === "open_notification_settings")).toHaveLength(1);
    });
  });

  it("turns the switch back off when the permission is refused, with a line pointing to System Settings", async () => {
    const calls = app({ notifications: { permission: "refused" } });
    render(<App store={createDaemonStore()} />);
    await openSettings();
    const notify = await screen.findByRole("switch", { name: "Notify on failure" });

    fireEvent.click(notify);

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Notifications are not allowed for Procly, so Notify on failure stays off. Allow them in System Settings > Notifications",
    );
    expect(notify).not.toBeChecked();
    expect(notificationCalls(calls)).toHaveLength(1);
  });

  it("turns the switch off without asking again", async () => {
    const calls = app({
      settings: { editor: null, terminal: "Terminal", show_in_dock: true, notify_on_failure: true },
    });
    render(<App store={createDaemonStore()} />);
    await openSettings();
    const notify = await screen.findByRole("switch", { name: "Notify on failure" });
    await waitFor(() => {
      expect(notify).toBeChecked();
    });

    fireEvent.click(notify);

    await waitFor(() => {
      expect(notify).not.toBeChecked();
    });
    expect(notificationCalls(calls)).toEqual([
      { command: "set_notify_on_failure", args: { enabled: false } },
    ]);
  });

  it("never sends the notification switch with the other settings, which would skip the permission", async () => {
    const calls = app();
    render(<App store={createDaemonStore()} />);
    await openSettings();
    fireEvent.click(await screen.findByRole("switch", { name: "Notify on failure" }));
    await waitFor(() => {
      expect(screen.getByRole("switch", { name: "Notify on failure" })).toBeChecked();
    });

    fireEvent.click(screen.getByRole("switch", { name: "Show in Dock while the window is open" }));

    await waitFor(() => {
      expect(saved(calls)).toHaveLength(1);
    });
    expect(JSON.stringify(saved(calls))).not.toContain("notify");
  });

  it("says why when the permission cannot be asked, and keeps the switch off", async () => {
    app({
      notifications: {
        failure: { kind: "notifications", message: "cannot ask for the permission to notify: no answer" },
      },
    });
    render(<App store={createDaemonStore()} />);
    await openSettings();
    const notify = await screen.findByRole("switch", { name: "Notify on failure" });

    fireEvent.click(notify);

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "cannot ask for the permission to notify: no answer",
    );
    expect(notify).not.toBeChecked();
  });

  it("says why when the notification settings do not open", async () => {
    app({
      notifications: {
        pane: () =>
          Promise.reject({
            kind: "notifications",
            message: "cannot open the notification settings of macOS: denied",
          }),
      },
    });
    render(<App store={createDaemonStore()} />);
    await openSettings();

    fireEvent.click(await screen.findByRole("button", { name: "Open notification settings" }));

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "cannot open the notification settings of macOS: denied",
    );
  });
});

describe("a clicked banner", () => {
  afterEach(() => {
    cleanup();
    clearMocks();
  });

  it("brings the window back on the failed Job, selected in the inspector of its Worktree", async () => {
    app();
    render(<App store={createDaemonStore()} />);
    await screen.findByRole("button", { name: "web-fix" });

    await act(async () => {
      await emit("open-failure", { kind: "job", worktree: "/p/work/acme/web-fix", id: "a4e2ea77" });
    });

    const inspector = await screen.findByRole("complementary", { name: "Inspector" });
    expect(await within(inspector).findByRole("heading", { name: "sh -c 'exit 3'" })).toBeInTheDocument();
    expect(within(inspector).getByText("a4e2ea77")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "web-fix" })).toHaveAttribute("aria-current", "true");
  });

  it("brings the window back on the failed Service, selected in the inspector of its Worktree", async () => {
    app();
    render(<App store={createDaemonStore()} />);
    await screen.findByRole("button", { name: /^main/ });

    await act(async () => {
      await emit("open-failure", { kind: "service", worktree: "/p/work/acme/web", service: "api" });
    });

    const inspector = await screen.findByRole("complementary", { name: "Inspector" });
    expect(await within(inspector).findByRole("heading", { name: "api" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /^main/ })).toHaveAttribute("aria-current", "true");
  });

  it("brings the window back on the Worktree of a Foreign process, which the inspector does not explain", async () => {
    app();
    render(<App store={createDaemonStore()} />);
    await screen.findByRole("button", { name: "web-fix" });

    await act(async () => {
      await emit("open-failure", { kind: "worktree", worktree: "/p/work/acme/web-fix" });
    });

    expect(await screen.findByRole("complementary", { name: "Inspector" })).toHaveTextContent(
      "/p/work/acme/web-fix",
    );
    expect(screen.getByRole("button", { name: "web-fix" })).toHaveAttribute("aria-current", "true");
  });

  it("shows the Overview when the banner names no Worktree, as a grouped banner does", async () => {
    app();
    render(<App store={createDaemonStore()} />);
    fireEvent.click(await screen.findByRole("button", { name: "web-fix" }));
    await screen.findByRole("complementary", { name: "Inspector" });

    await act(async () => {
      await emit("open-failure", null);
    });

    expect(await screen.findByRole("region", { name: "Overview" })).toBeInTheDocument();
  });
});

describe("opening a Worktree", () => {
  afterEach(() => {
    cleanup();
    clearMocks();
  });

  async function selectWorktree(): Promise<void> {
    fireEvent.click(await screen.findByRole("button", { name: "web-fix" }));
    await screen.findByRole("complementary", { name: "Inspector" });
  }

  it("asks Rust to open the Worktree in the terminal, by its path", async () => {
    const calls = app();
    render(<App store={createDaemonStore()} />);
    await selectWorktree();

    fireEvent.click(screen.getByRole("button", { name: "Open in terminal" }));

    await waitFor(() => {
      expect(calls.filter((call) => call.command === "open_worktree")).toEqual([
        { command: "open_worktree", args: { path: "/p/work/acme/web-fix", with: "terminal" } },
      ]);
    });
  });

  it("asks Rust to open it in the editor, and to reveal it in the Finder with the same path", async () => {
    const calls = app();
    render(<App store={createDaemonStore()} />);
    await selectWorktree();

    fireEvent.click(screen.getByRole("button", { name: "Open in editor" }));
    fireEvent.click(screen.getByRole("button", { name: "Reveal in Finder" }));

    await waitFor(() => {
      expect(calls.filter((call) => call.command.endsWith("_worktree"))).toEqual([
        { command: "open_worktree", args: { path: "/p/work/acme/web-fix", with: "editor" } },
        { command: "reveal_worktree", args: { path: "/p/work/acme/web-fix" } },
      ]);
    });
  });

  it("opens Settings on the editor field when no editor is set", async () => {
    app({
      opening: (command) =>
        command === "open_worktree"
          ? Promise.reject({ kind: "no_editor", message: "no editor is set, choose one in Settings" })
          : undefined,
    });
    render(<App store={createDaemonStore()} />);
    await selectWorktree();

    fireEvent.click(screen.getByRole("button", { name: "Open in editor" }));

    const editor = await screen.findByRole("combobox", { name: "Editor" });
    expect(screen.getByRole("heading", { name: "Open with" })).toBeInTheDocument();
    await waitFor(() => {
      expect(editor).toHaveFocus();
    });
  });

  it("shows the error that names the app when it does not open, and stays on the Worktree", async () => {
    app({
      opening: () =>
        Promise.reject({
          kind: "launch",
          message: "cannot open /p/work/acme/web-fix with Ghostty: Unable to find application",
        }),
    });
    render(<App store={createDaemonStore()} />);
    await selectWorktree();

    fireEvent.click(screen.getByRole("button", { name: "Open in terminal" }));

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "cannot open /p/work/acme/web-fix with Ghostty: Unable to find application",
    );
    expect(screen.getByRole("complementary", { name: "Inspector" })).toBeInTheDocument();
  });

  it("drops an error when another Worktree is selected", async () => {
    app({ opening: () => Promise.reject({ kind: "launch", message: "cannot open /p/work/acme/web-fix" }) });
    render(<App store={createDaemonStore()} />);
    await selectWorktree();
    fireEvent.click(screen.getByRole("button", { name: "Open in terminal" }));
    await screen.findByRole("alert");

    fireEvent.click(screen.getByRole("button", { name: /^main\s*main$/ }));

    await waitFor(() => {
      expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    });
  });
});
