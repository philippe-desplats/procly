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
    settings?: Settings;
    apps?: DetectedApps;
    opening?: (command: string, args: unknown) => unknown;
    saving?: () => unknown;
  } = {},
): Call[] {
  const calls: Call[] = [];
  mockIPC(
    (command, args) => {
      calls.push({ command, args });
      switch (command) {
        case "last_update":
          return FIXED;
        case "get_settings":
          return options.settings ?? { editor: null, terminal: "Terminal" };
        case "detected_apps":
          return options.apps ?? APPS;
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
    app({ settings: { editor: "Cursor", terminal: "iTerm" } });
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
    const calls = app({ settings: { editor: null, terminal: "iTerm" } });
    render(<App store={createDaemonStore()} />);
    await openSettings();
    const editor = await screen.findByRole("combobox", { name: "Editor" });
    await waitFor(() => {
      expect(screen.getByRole("combobox", { name: "Terminal" })).toHaveValue("iTerm");
    });

    fireEvent.change(editor, { target: { value: "Zed" } });

    await waitFor(() => {
      expect(saved(calls)).toEqual([{ settings: { editor: "Zed", terminal: "iTerm" } }]);
    });
    expect(screen.queryByRole("button", { name: /save/i })).not.toBeInTheDocument();

    fireEvent.change(screen.getByRole("combobox", { name: "Terminal" }), { target: { value: "Ghostty" } });

    await waitFor(() => {
      expect(saved(calls).at(-1)).toEqual({ settings: { editor: "Zed", terminal: "Ghostty" } });
    });
  });

  it("takes None as no editor", async () => {
    const calls = app({ settings: { editor: "Zed", terminal: "Terminal" } });
    render(<App store={createDaemonStore()} />);
    await openSettings();
    const editor = await screen.findByRole("combobox", { name: "Editor" });
    await waitFor(() => {
      expect(editor).toHaveValue("Zed");
    });

    fireEvent.change(editor, { target: { value: "" } });

    await waitFor(() => {
      expect(saved(calls)).toEqual([{ settings: { editor: null, terminal: "Terminal" } }]);
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
      expect(saved(calls)).toEqual([{ settings: { editor: "Nova", terminal: "Terminal" } }]);
    });
  });

  it("shows an app that was typed and is not detected as Other, with its name", async () => {
    app({ settings: { editor: "Nova", terminal: "Terminal" } });
    render(<App store={createDaemonStore()} />);

    await openSettings();

    expect(await screen.findByRole("textbox", { name: "Editor app name" })).toHaveValue("Nova");
    expect(screen.getByRole("combobox", { name: "Editor" })).toHaveValue("__other__");
  });

  it("keeps the default terminal when the name typed is cleared", async () => {
    const calls = app({ settings: { editor: null, terminal: "Alacritty" } });
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
