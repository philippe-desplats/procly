/** A command to copy: the box shows exactly what the Copy button puts on the clipboard. */
export function CopyBox({ text }: { text: string }) {
  return (
    <div className="copybox">
      <code className="mono">{text}</code>
      <button
        type="button"
        className="button"
        onClick={() => {
          void navigator.clipboard.writeText(text);
        }}
      >
        Copy
      </button>
    </div>
  );
}
