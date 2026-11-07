/**
 * A command to copy: the box shows exactly what the Copy button puts on the clipboard, and keeps the line breaks of a
 * text that spans lines, such as a prompt. Every copy box of the app is this one, in one style.
 */
export function CopyBox({ text }: { text: string }) {
  return (
    <div className="copybox">
      <code className="mono">{text}</code>
      <button
        type="button"
        className="copybox-button"
        onClick={() => {
          void navigator.clipboard.writeText(text);
        }}
      >
        Copy
      </button>
    </div>
  );
}
