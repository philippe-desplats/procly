/**
 * The mark of Procly, on the 512 by 512 grid of the app icon. Its three colours come from the brand tokens of the
 * stylesheet, so the mark has one place to change. It names itself, since on a state screen it is the only picture.
 */
export function BrandMark() {
  return (
    <svg className="brand-mark" viewBox="0 0 512 512" role="img" aria-label="Procly">
      <rect className="brand-ink" width="512" height="512" rx="116" />
      <path
        className="brand-lime-deep"
        fillRule="evenodd"
        d="M204 100H412V308L308 412H100V204ZM256 152H308L100 360V308ZM360 204V256L204 412H152Z"
      />
      <path className="brand-lime" d="M204 100H412V308L360 360V152H152Z" />
    </svg>
  );
}
