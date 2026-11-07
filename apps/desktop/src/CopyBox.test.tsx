import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import { CopyBox } from "./CopyBox";

afterEach(cleanup);

describe("a copy box", () => {
  it("shows a command with a Copy button that is not a bordered button", () => {
    render(<CopyBox text="procly unit install" />);

    const copy = screen.getByRole("button", { name: "Copy" });

    expect(copy).toHaveClass("copybox-button");
    expect(copy).not.toHaveClass("button");
    expect(copy.closest(".copybox")).toHaveTextContent("procly unit install");
  });

  it("copies exactly the text it shows", () => {
    const written: string[] = [];
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: {
        writeText: (text: string) => {
          written.push(text);
          return Promise.resolve();
        },
      },
    });
    render(<CopyBox text="procly daemon stop" />);

    fireEvent.click(screen.getByRole("button", { name: "Copy" }));

    expect(written).toEqual(["procly daemon stop"]);
  });
});
