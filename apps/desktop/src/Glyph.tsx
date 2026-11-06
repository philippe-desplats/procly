import type { ReactElement } from "react";

import type { Glyph } from "./state";

/**
 * The inside of each glyph, on a 12 by 12 grid. A shape is told apart by its outline alone: the colour comes from the
 * stylesheet through `currentColor`, so a person who cannot tell red, amber and green apart still reads the state.
 */
function shape(glyph: Glyph): ReactElement {
  switch (glyph) {
    case "healthy":
      return <circle cx="6" cy="6" r="3.6" fill="currentColor" />;
    case "ended":
      return <circle cx="6" cy="6" r="3.6" fill="none" stroke="currentColor" strokeWidth="1.4" />;
    case "busy":
      return (
        <>
          <circle cx="6" cy="6" r="4.2" fill="none" stroke="currentColor" strokeWidth="1.6" opacity="0.3" />
          <path
            className="spin"
            d="M6 1.8A4.2 4.2 0 0 1 10.2 6"
            fill="none"
            stroke="currentColor"
            strokeWidth="1.6"
            strokeLinecap="round"
          />
        </>
      );
    case "failure":
      return (
        <>
          <circle cx="6" cy="6" r="5.4" fill="currentColor" />
          <path className="glyph-cut" d="M4 4l4 4M8 4l-4 4" strokeWidth="1.5" strokeLinecap="round" />
        </>
      );
    case "job":
      return (
        <>
          <circle cx="6" cy="6" r="4.2" fill="none" stroke="currentColor" strokeWidth="1.4" />
          <path d="M6 1.8A4.2 4.2 0 0 1 6 10.2Z" fill="currentColor" />
        </>
      );
    case "foreign":
      return <path d="M6 1l5 5-5 5-5-5z" fill="currentColor" />;
    case "passed":
      return (
        <path
          d="M2.5 6.2l2.4 2.4 4.8-5"
          fill="none"
          stroke="currentColor"
          strokeWidth="1.7"
          strokeLinecap="round"
          strokeLinejoin="round"
        />
      );
    case "skipped":
      return <path d="M3 6h6" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" />;
    case "warning":
      return (
        <>
          <path d="M6 1l5.3 9.5H.7z" fill="currentColor" />
          <path className="glyph-cut" d="M6 4.5v2.6" strokeWidth="1.4" strokeLinecap="round" />
          <circle className="glyph-cut-fill" cx="6" cy="8.8" r="0.8" />
        </>
      );
    default: {
      const unhandled: never = glyph;
      throw new Error(`unhandled glyph: ${String(unhandled)}`);
    }
  }
}

/** The shape of a state. Decorative: the state word beside it is what a screen reader reads. */
export function GlyphMark({ glyph }: { glyph: Glyph }) {
  return (
    <svg className="glyph" data-glyph={glyph} viewBox="0 0 12 12" aria-hidden="true" focusable="false">
      {shape(glyph)}
    </svg>
  );
}
