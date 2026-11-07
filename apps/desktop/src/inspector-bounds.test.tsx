import { readFileSync } from "node:fs";
import { join } from "node:path";

import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import type { Channel } from "@tauri-apps/api/core";
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { afterAll, afterEach, beforeAll, describe, expect, it } from "vitest";

import { App } from "./App";
import { createDaemonStore, type Update } from "./daemon";
import recorded from "./fixtures/status.json";
import type { Response } from "./protocol/Response";
import type { StatusReport } from "./protocol/StatusReport";

/**
 * jsdom lays nothing out, so it cannot measure how tall the output is. What it can do is apply the real stylesheet to
 * the real inspector and read the computed style of every box between the output and the pane, which is where the
 * height of the output is bounded. The heights themselves were measured in a real engine when the rules were written.
 */
// Vitest hands an empty string for a stylesheet imported as text, so the file is read, from the root of the project.
const css = readFileSync(join(process.cwd(), "src", "styles.css"), "utf8");

const status = recorded as unknown as StatusReport & { response: "status" };
const LINES = 400;

let style: HTMLStyleElement;

beforeAll(() => {
  expect(css).toContain(".output-box");
  style = document.createElement("style");
  style.textContent = css;
  document.head.append(style);
});

afterAll(() => {
  style.remove();
});

afterEach(() => {
  cleanup();
  clearMocks();
});

/** The inspector of the `api` Service, following an output of 400 lines. */
async function openLongOutput(): Promise<HTMLElement> {
  const update: Update = {
    at_ms: Date.UTC(2026, 10, 5, 19, 35, 0),
    response: status,
    needs_you: [],
    seen: [],
  };
  let channel: Channel<Response> | null = null;
  mockIPC(
    (command, payload) => {
      const args = (payload ?? {}) as Record<string, unknown>;
      switch (command) {
        case "last_update":
          return update;
        case "follow_logs":
          channel = args["onOutput"] as Channel<Response>;
          return new Promise<void>(() => undefined);
        case "refresh":
        case "stop_follow":
          return undefined;
        default:
          return Promise.reject(new Error(`unexpected command ${command}`));
      }
    },
    { shouldMockEvents: true },
  );
  render(<App store={createDaemonStore()} />);
  await screen.findByRole("heading", { name: "Overview" });
  fireEvent.click(screen.getByRole("button", { name: /^api/ }));
  await waitFor(() => {
    expect(channel).not.toBeNull();
  });
  const output = Array.from({ length: LINES }, (_, index) => `chatty line ${String(index)}`).join("\n");
  await act(async () => {
    (channel as unknown as Channel<Response>).onmessage({ response: "logs", output: `${output}\n` });
    await Promise.resolve();
  });
  const inspector = screen.getByRole("complementary", { name: "Inspector" });
  await within(inspector).findByText(`chatty line ${String(LINES - 1)}`);
  return inspector;
}

const computed = (element: Element) => getComputedStyle(element);

/** The boxes between the output and the pane that scrolls, both excluded. */
function ancestors(output: Element, inspector: Element): Element[] {
  const boxes: Element[] = [];
  for (let box = output.parentElement; box !== null && box !== inspector; box = box.parentElement) {
    boxes.push(box);
  }
  return boxes;
}

/**
 * Why this proves the output is bounded. The height of a column of flex items is the sum of what its items ask for, and
 * a `pre` in the flow asks for the height of its text: 400 lines made it 6393 px tall, the inspector 7163 px, and put
 * the Open buttons 6218 px under its bottom. The output is therefore bounded by 2 things together, read here from the
 * computed style of the real boxes:
 * - the `pre` is out of flow (`position: absolute`) inside its box, so no number of lines can make anything taller,
 * - the box that holds it asks for 0 px plus a floor (`flex-basis: 0px`, `min-height`), grows into what is left, and
 *   scrolls the `pre` (`overflow: auto`).
 * Putting the `pre` back in the flow, or giving its box a content-sized basis, fails one of the assertions below.
 */
describe("the output of a Service in the inspector", () => {
  it("is out of the flow and scrolls inside a box that asks for a floor and grows into what is left", async () => {
    const inspector = await openLongOutput();
    const output = within(inspector).getByRole("log", { name: "Output" });
    const box = output.parentElement as HTMLElement;

    expect(box).toHaveClass("output-box");
    expect(computed(output).position).toBe("absolute");
    expect(computed(output).overflow).toBe("auto");
    expect(computed(box).position).toBe("relative");
    expect(computed(box).flexBasis).toBe("0px");
    expect(computed(box).flexGrow).toBe("1");
    expect(computed(box).minHeight).toBe("120px");
  });

  it("sits in boxes that are columns all the way to the pane, so the pane scrolls and the output does not grow it", async () => {
    const inspector = await openLongOutput();
    const output = within(inspector).getByRole("log", { name: "Output" });

    expect(inspector).toHaveClass("inspector");
    expect(computed(inspector).display).toBe("flex");
    expect(computed(inspector).flexDirection).toBe("column");
    expect(computed(inspector).overflow).toBe("auto");
    const columns = ancestors(output, inspector).filter((box) => !box.classList.contains("output-box"));
    expect(columns.map((box) => box.className)).toEqual(["output-pane", "inspector-panel"]);
    for (const box of columns) {
      expect(computed(box).display).toBe("flex");
      expect(computed(box).flexDirection).toBe("column");
    }
    // The pane asks for its content, which is the output's floor and not its text, and grows into what is left.
    const pane = columns[0] as Element;
    expect(computed(pane).flexBasis).toBe("auto");
    expect(computed(pane).flexGrow).toBe("1");
  });

  it("stays bounded with Find revealed and Wrap on, and the Worktree actions follow the panel inside the pane", async () => {
    const inspector = await openLongOutput();
    const panel = within(inspector);

    fireEvent.click(panel.getByRole("button", { name: "Find" }));
    fireEvent.click(panel.getByRole("button", { name: "Wrap" }));

    const output = panel.getByRole("log", { name: "Output" });
    const box = output.parentElement as HTMLElement;
    expect(output).toHaveClass("wrap");
    expect(panel.getByRole("searchbox", { name: "Find in output" })).toBeInTheDocument();
    expect(computed(output).position).toBe("absolute");
    expect(computed(box).flexBasis).toBe("0px");
    expect(computed(box).minHeight).toBe("120px");
    // Open in Editor and Open in Terminal moved to the title line of the Worktree, Reveal in Finder stays here.
    const buttons = panel.getByRole("button", { name: "Reveal in Finder" }).parentElement as HTMLElement;
    expect(buttons).toHaveClass("worktree-actions");
    expect(buttons.parentElement).toBe(inspector);
    expect(buttons.previousElementSibling).toHaveClass("inspector-panel");
  });
});
