import type { StateWord } from "./state";

/** A state as a coloured dot and a word: the colour only repeats what the word says. */
export function State({ word, tone }: StateWord) {
  return (
    <span className={`state tone-${tone}`}>
      <span className="dot" aria-hidden="true" />
      {word}
    </span>
  );
}
