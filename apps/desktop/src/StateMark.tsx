import { GlyphMark } from "./Glyph";
import type { StateWord } from "./state";

/** A state as a glyph and a word: the shape of the glyph and the word say the state, the colour only repeats it. */
export function State({ word, tone, glyph }: StateWord) {
  return (
    <span className={`state tone-${tone}`}>
      <GlyphMark glyph={glyph} />
      {word}
    </span>
  );
}
