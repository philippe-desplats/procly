import { GlyphMark } from "./Glyph";
import type { StateWord } from "./state";

/** The glyph of a state alone, for a row that opens with it and draws the word later, after the name. */
export function StateGlyph({ tone, glyph }: Pick<StateWord, "tone" | "glyph">) {
  return (
    <span className={`state-glyph tone-${tone}`}>
      <GlyphMark glyph={glyph} />
    </span>
  );
}

/** A state as a glyph and a word: the shape of the glyph and the word say the state, the colour only repeats it. */
export function State({ word, tone, glyph }: StateWord) {
  return (
    <span className={`state tone-${tone}`}>
      <GlyphMark glyph={glyph} />
      <span className="word">{word}</span>
    </span>
  );
}
