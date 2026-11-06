/**
 * A command to copy: the box shows exactly what the Copy button puts on the clipboard. `block` keeps the line breaks
 * of a text that spans lines, such as a prompt.
 */
export function CopyBox({ text, block = false }: { text: string; block?: boolean }) {
  return (
    <div className={block ? "copybox copybox-block" : "copybox"}>
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
