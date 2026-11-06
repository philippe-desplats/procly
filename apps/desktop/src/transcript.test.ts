import { describe, expect, it } from "vitest";

import { MAX_BYTES, Transcript, utf8Length, type Line } from "./transcript";

/** The text of each line, to say what the developer reads. */
function texts(transcript: Transcript): string[] {
  return transcript.lines().map((line) => line.spans.map((span) => span.text).join(""));
}

/** The styled runs of one line, without the ids. */
function runs(line: Line | undefined): { text: string; colour: string | null; bold: boolean }[] {
  return (line?.spans ?? []).map(({ text, colour, bold }) => ({ text, colour, bold }));
}

function written(...chunks: string[]): Transcript {
  const transcript = new Transcript();
  for (const chunk of chunks) {
    transcript.write(chunk);
  }
  return transcript;
}

describe("the text of the output", () => {
  it("is one line per newline, the last one still open", () => {
    expect(texts(written("ready\nlistening on 3000\nwait"))).toEqual(["ready", "listening on 3000", "wait"]);
  });

  it("keeps tabs and drops the other control characters", () => {
    expect(texts(written("a\tb\x07c\x08d\x7fe\n"))).toEqual(["a\tbcde", ""]);
  });
});

describe("the colours and the bold", () => {
  it("draws a colour until the reset", () => {
    const [line] = written("\x1b[31merror\x1b[0m ok").lines();

    expect(runs(line)).toEqual([
      { text: "error", colour: "red", bold: false },
      { text: " ok", colour: null, bold: false },
    ]);
  });

  it("combines bold and a colour in one sequence, and ends each on its own", () => {
    const [line] = written("\x1b[1;32mok\x1b[22m green\x1b[39m plain \x1b[m").lines();

    expect(runs(line)).toEqual([
      { text: "ok", colour: "green", bold: true },
      { text: " green", colour: "green", bold: false },
      { text: " plain ", colour: null, bold: false },
    ]);
  });

  it("reads the 8 colours, their bright forms and the grey", () => {
    const colours = [30, 31, 32, 33, 34, 35, 36, 37, 90, 91, 97].map((code) => {
      const [line] = written(`\x1b[${code}mx`).lines();
      return runs(line)[0]?.colour;
    });

    expect(colours).toEqual([
      "black",
      "red",
      "green",
      "yellow",
      "blue",
      "magenta",
      "cyan",
      "white",
      "grey",
      "red",
      "white",
    ]);
  });

  it("keeps its style across the chunks", () => {
    const [line] = written("\x1b[33mwarn", "ing\x1b[0m").lines();

    expect(runs(line)).toEqual([{ text: "warning", colour: "yellow", bold: false }]);
  });

  it("reads an extended colour whole, so its numbers are not taken for bold or a colour", () => {
    const [indexed] = written("\x1b[38;5;1mx").lines();
    const [direct] = written("\x1b[38;2;1;22;33my").lines();
    const [background] = written("\x1b[48;5;1;1mz").lines();

    expect(runs(indexed)).toEqual([{ text: "x", colour: null, bold: false }]);
    expect(runs(direct)).toEqual([{ text: "y", colour: null, bold: false }]);
    // The `1` after the background is bold, the 5 and the 1 before it are its colour.
    expect(runs(background)).toEqual([{ text: "z", colour: null, bold: true }]);
  });
});

describe("the escape sequences that are not colours", () => {
  it("removes cursor and erase sequences", () => {
    expect(texts(written("\x1b[2K\x1b[1Gloading\x1b[?25l\x1b[0K done\x1b[?25h"))).toEqual(["loading done"]);
  });

  it("removes a title set by an OSC sequence, ended by BEL or by ST", () => {
    expect(texts(written("a\x1b]0;title\x07b\x1b]8;;http://example.com\x1b\\c"))).toEqual(["abc"]);
  });

  it("removes the other escape sequences, such as the choice of a character set", () => {
    expect(texts(written("a\x1b(Bb\x1b=c\x1bPdevice\x1b\\d"))).toEqual(["abcd"]);
  });

  it("removes a sequence split between 2 chunks", () => {
    expect(texts(written("a\x1b[", "2Kb\x1b", "]0;ti", "tle\x07c"))).toEqual(["abc"]);
  });

  it("does not keep a sequence that stops short at a newline", () => {
    expect(texts(written("a\x1b[3\nb"))).toEqual(["a", "b"]);
  });
});

describe("the carriage return", () => {
  it("rewrites the current line, as a progress bar does", () => {
    expect(texts(written("10%\r20%\r30%"))).toEqual(["30%"]);
  });

  it("rewrites the line with what comes after it in another chunk", () => {
    expect(texts(written("downloading 1/3\r", "downloading 2/3\r", "downloading 3/3\n", "done"))).toEqual([
      "downloading 3/3",
      "done",
    ]);
  });

  it("is the end of a line, not a rewrite, before a newline, whether or not they share a chunk", () => {
    expect(texts(written("one\r\ntwo\r", "\nthree"))).toEqual(["one", "two", "three"]);
  });

  it("keeps the colour that was set before the rewrite", () => {
    const [line] = written("\x1b[31m10%\r20%").lines();

    expect(runs(line)).toEqual([{ text: "20%", colour: "red", bold: false }]);
  });
});

describe("the cap", () => {
  const line = `${"x".repeat(99)}\n`;

  it("counts bytes, not letters", () => {
    expect(utf8Length("abc")).toBe(3);
    expect(utf8Length("é")).toBe(2);
    expect(utf8Length("€")).toBe(3);
    expect(utf8Length("😀")).toBe(4);
  });

  it("keeps at most 1 MiB, dropping the oldest lines first", () => {
    const transcript = new Transcript();
    for (let index = 0; index < 30_000; index += 1) {
      transcript.write(`${String(index).padStart(5, "0")} ${line}`);
    }

    const kept = texts(transcript);
    expect(transcript.bytes).toBeLessThanOrEqual(MAX_BYTES);
    expect(transcript.bytes).toBeGreaterThan(MAX_BYTES - 200);
    expect(kept.at(-2)?.startsWith("29999 ")).toBe(true);
    expect(kept[0]?.startsWith("00000 ")).toBe(false);
    // What remains is the newest text, in order and whole.
    expect(kept.slice(0, -1).every((text) => text.length === 105)).toBe(true);
  });

  it("keeps the end of a single line that is longer than the cap", () => {
    const transcript = new Transcript();

    transcript.write(`${"a".repeat(MAX_BYTES)}${"b".repeat(1000)}`);

    const [only] = texts(transcript);
    expect(texts(transcript)).toHaveLength(1);
    expect(transcript.bytes).toBeLessThanOrEqual(MAX_BYTES);
    expect(only?.endsWith("b".repeat(1000))).toBe(true);
  });

  it("does not split a character when it trims", () => {
    const transcript = new Transcript();

    transcript.write("😀".repeat(MAX_BYTES / 4 + 10));

    const [only] = texts(transcript);
    expect(transcript.bytes).toBeLessThanOrEqual(MAX_BYTES);
    expect(only?.length).toBe((MAX_BYTES / 4) * 2);
  });
});

describe("the output that was dropped", () => {
  it("leaves a note of its size where it was", () => {
    const transcript = written("before\n");

    transcript.skipped(4096);
    transcript.write("after\n");

    expect(texts(transcript)).toEqual(["before", "4096 bytes of output were skipped", "after", ""]);
    expect(transcript.lines()[1]?.note).toBe(true);
  });
});

describe("the lines it hands over", () => {
  it("keep their identity while they do not change, so that the window redraws only the last one", () => {
    const transcript = written("one\ntwo\nthr");
    const before = transcript.lines();

    transcript.write("ee");
    const after = transcript.lines();

    expect(after[0]).toBe(before[0]);
    expect(after[1]).toBe(before[1]);
    expect(after[2]).not.toBe(before[2]);
    expect(after[2]?.id).toBe(before[2]?.id);
  });
});
