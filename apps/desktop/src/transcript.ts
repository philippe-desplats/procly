/**
 * The output of a Service or a Job as the inspector shows it: lines of styled runs, built chunk by chunk as the Daemon
 * sends them. The colours and the bold of SGR sequences are kept, `\r` rewrites the current line, every other escape
 * sequence is removed, and at most 1 MiB is kept, the newest.
 *
 * The output is untrusted text. Nothing here builds markup, the window draws the runs as text.
 */

/** What the transcript keeps at most, in UTF-8 bytes, newlines included. */
export const MAX_BYTES = 1024 * 1024;

/** The colours the window draws, each one a token of its palette. A bright colour is drawn as its plain one. */
export type Colour = "black" | "red" | "green" | "yellow" | "blue" | "magenta" | "cyan" | "white" | "grey";

/** A run of text in one style. */
export interface Span {
  text: string;
  colour: Colour | null;
  bold: boolean;
}

/** One line. A line that changes is replaced by a new object with the same `id`, so the window redraws only it. */
export interface Line {
  id: number;
  spans: readonly Span[];
  /** Whether it is a note of the app and not output. */
  note?: boolean;
  /** The UTF-8 size of its text. */
  bytes: number;
}

const COLOURS: Record<number, Colour> = {
  30: "black",
  31: "red",
  32: "green",
  33: "yellow",
  34: "blue",
  35: "magenta",
  36: "cyan",
  37: "white",
  90: "grey",
  91: "red",
  92: "green",
  93: "yellow",
  94: "blue",
  95: "magenta",
  96: "cyan",
  97: "white",
};

/** The size of `text` once encoded as UTF-8. */
export function utf8Length(text: string): number {
  let bytes = 0;
  for (let index = 0; index < text.length; index += 1) {
    const unit = text.charCodeAt(index);
    if (unit < 0x80) {
      bytes += 1;
    } else if (unit < 0x800) {
      bytes += 2;
    } else if (unit >= 0xd800 && unit < 0xdc00 && index + 1 < text.length) {
      // A pair of surrogates is one character of 4 bytes.
      bytes += 4;
      index += 1;
    } else {
      bytes += 3;
    }
  }
  return bytes;
}

/** Where the reader of the characters stands in an escape sequence. */
type Mode =
  | "text"
  /** After ESC. */
  | "escape"
  /** In a sequence that ends on any final byte, such as the choice of a character set. */
  | "intermediate"
  /** In the parameters of a CSI sequence. */
  | "csi"
  /** In a string sequence (OSC, DCS and the like), which ends on BEL or on ESC then `\`. */
  | "string"
  /** In a string sequence, just after an ESC. */
  | "string-escape";

const ESC = "\x1b";
const BEL = "\x07";

/** Cuts `drop` bytes off the start of the spans, never in the middle of a character. */
function trimStart(spans: readonly Span[], drop: number): Span[] {
  const kept: Span[] = [];
  let left = drop;
  for (const span of spans) {
    if (left <= 0) {
      kept.push(span);
      continue;
    }
    let cut = 0;
    let removed = 0;
    for (const character of span.text) {
      if (removed >= left) {
        break;
      }
      removed += utf8Length(character);
      cut += character.length;
    }
    left -= removed;
    if (cut < span.text.length) {
      kept.push({ ...span, text: span.text.slice(cut) });
    }
  }
  return kept;
}

export class Transcript {
  readonly #limit: number;
  #lines: Line[] = [];
  #nextId = 0;
  /** The bytes of the lines and of the newlines between them. */
  #bytes = 0;
  #colour: Colour | null = null;
  #bold = false;
  /** After a `\r`: the next character written replaces the line. */
  #rewrite = false;
  #mode: Mode = "text";
  #parameters = "";

  constructor(limit: number = MAX_BYTES) {
    this.#limit = limit;
    this.#open();
  }

  /** What the transcript keeps, in UTF-8 bytes. */
  get bytes(): number {
    return this.#bytes;
  }

  /** The lines, the last one still open. The array is new, the lines that did not change are the same objects. */
  lines(): Line[] {
    return [...this.#lines];
  }

  /** Reads a chunk of output. A sequence or a `\r\n` may be split between two chunks. */
  write(chunk: string): void {
    let run = "";
    const flush = (): void => {
      if (run !== "") {
        this.#append(run);
        run = "";
      }
    };
    for (const character of chunk) {
      if (this.#mode === "text") {
        if (character === ESC) {
          flush();
          this.#mode = "escape";
        } else if (character === "\n") {
          flush();
          this.#newline();
        } else if (character === "\r") {
          flush();
          this.#rewrite = true;
        } else if (character === "\t" || (character >= " " && character !== "\x7f")) {
          run += character;
        }
        // Any other control character is dropped.
        continue;
      }
      // Inside a sequence a control character ends it and counts as itself.
      if (this.#mode !== "string" && this.#mode !== "string-escape" && character < " " && character !== ESC) {
        this.#mode = "text";
        if (character === "\n") {
          this.#newline();
        } else if (character === "\r") {
          this.#rewrite = true;
        }
        continue;
      }
      this.#escape(character);
    }
    flush();
  }

  /** Notes that `bytes` of output never reached the window, where they would have been. */
  skipped(bytes: number): void {
    this.#note(`${String(bytes)} bytes of output were skipped`);
  }

  #escape(character: string): void {
    switch (this.#mode) {
      case "escape":
        if (character === "[") {
          this.#mode = "csi";
          this.#parameters = "";
        } else if (
          character === "]" ||
          character === "P" ||
          character === "X" ||
          character === "^" ||
          character === "_"
        ) {
          this.#mode = "string";
        } else if (character >= " " && character <= "/") {
          this.#mode = "intermediate";
        } else {
          this.#mode = "text";
        }
        return;
      case "intermediate":
        if (character < " " || character > "/") {
          this.#mode = "text";
        }
        return;
      case "csi":
        if (character === ESC) {
          this.#mode = "escape";
        } else if (character >= "@" && character <= "~") {
          if (character === "m") {
            this.#sgr(this.#parameters);
          }
          this.#mode = "text";
        } else {
          this.#parameters += character;
        }
        return;
      case "string":
        if (character === BEL) {
          this.#mode = "text";
        } else if (character === ESC) {
          this.#mode = "string-escape";
        }
        return;
      case "string-escape":
        // ESC then `\` ends the string, ESC then anything else starts a sequence of its own.
        this.#mode = character === "\\" ? "text" : "escape";
        if (this.#mode === "escape") {
          this.#escape(character);
        }
        return;
      case "text":
        return;
      default: {
        const unhandled: never = this.#mode;
        throw new Error(`unhandled mode: ${String(unhandled)}`);
      }
    }
  }

  /** Applies the parameters of a `CSI ... m` sequence. Anything it does not draw is skipped. */
  #sgr(parameters: string): void {
    if (!/^[0-9;:]*$/.test(parameters)) {
      return;
    }
    const codes = parameters.split(";");
    for (let index = 0; index < codes.length; index += 1) {
      const text = codes[index] ?? "";
      if (text.includes(":")) {
        // The colon form of an extended colour carries all its numbers in one parameter.
        continue;
      }
      const code = text === "" ? 0 : Number(text);
      if (code === 0) {
        this.#colour = null;
        this.#bold = false;
      } else if (code === 1) {
        this.#bold = true;
      } else if (code === 22) {
        this.#bold = false;
      } else if (code === 39) {
        this.#colour = null;
      } else if (code in COLOURS) {
        this.#colour = COLOURS[code] ?? null;
      } else if (code === 38 || code === 48) {
        // An extended colour is not in the palette, and its numbers are not codes of their own.
        const kind = Number(codes[index + 1] ?? "");
        index += kind === 5 ? 2 : kind === 2 ? 4 : 1;
      }
    }
  }

  #open(): void {
    this.#lines.push({ id: this.#nextId, spans: [], bytes: 0 });
    this.#nextId += 1;
  }

  #current(): Line {
    const line = this.#lines.at(-1);
    if (line === undefined) {
      throw new Error("a transcript always has an open line");
    }
    return line;
  }

  #replaceCurrent(line: Line): void {
    this.#lines[this.#lines.length - 1] = line;
  }

  #newline(): void {
    this.#rewrite = false;
    this.#open();
    this.#bytes += 1;
    this.#trim();
  }

  #append(text: string): void {
    let line = this.#current();
    if (this.#rewrite) {
      this.#rewrite = false;
      this.#bytes -= line.bytes;
      line = { id: line.id, spans: [], bytes: 0 };
    }
    const last = line.spans.at(-1);
    const size = utf8Length(text);
    const spans =
      last !== undefined && last.colour === this.#colour && last.bold === this.#bold
        ? [...line.spans.slice(0, -1), { ...last, text: last.text + text }]
        : [...line.spans, { text, colour: this.#colour, bold: this.#bold }];
    this.#replaceCurrent({ id: line.id, spans, bytes: line.bytes + size });
    this.#bytes += size;
    this.#trim();
  }

  #note(text: string): void {
    // A note stands on a line of its own, so the open line is closed first when it holds text.
    if (this.#current().bytes > 0) {
      this.#newline();
    }
    this.#replaceCurrent({
      id: this.#current().id,
      spans: [{ text, colour: null, bold: false }],
      note: true,
      bytes: utf8Length(text),
    });
    this.#bytes += utf8Length(text);
    this.#newline();
  }

  /** Drops the oldest lines past the cap, then the start of the line that is alone and still too long. */
  #trim(): void {
    while (this.#bytes > this.#limit && this.#lines.length > 1) {
      const dropped = this.#lines.shift();
      this.#bytes -= (dropped?.bytes ?? 0) + 1;
    }
    if (this.#bytes > this.#limit) {
      const line = this.#current();
      const spans = trimStart(line.spans, this.#bytes - this.#limit);
      const bytes = spans.reduce((sum, span) => sum + utf8Length(span.text), 0);
      this.#bytes -= line.bytes - bytes;
      this.#replaceCurrent({ ...line, spans, bytes });
    }
  }
}
