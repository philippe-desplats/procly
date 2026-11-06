import type { StateWord } from "./state";

/** A state in words, with the dot of its tone: colour only carries the state, the word says it. */
export function State({ word, tone }: StateWord) {
  return (
    <span className={`state tone-${tone}`}>
      <span className="dot" aria-hidden="true" />
      {word}
    </span>
  );
}
