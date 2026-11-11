/**
 * A port as a pill, the one drawing every screen uses, so that a port looks the same on a row and in the inspector.
 * Clicking it copies the port, as the shape of the app states. On a row the pill sits beside the button that picks the
 * row, never inside it, since a button holds no button.
 */
export function PortPill({ port }: { port: number }) {
  return (
    <button
      type="button"
      className="port mono"
      title="Copy port"
      aria-label={`Copy port ${String(port)}`}
      onClick={() => {
        void navigator.clipboard.writeText(String(port));
      }}
    >
      {String(port)}
    </button>
  );
}

/** The pills of the ports of one item, none when it holds none. */
export function PortPills({ ports }: { ports: number[] }) {
  return (
    <>
      {ports.map((port) => (
        <PortPill key={port} port={port} />
      ))}
    </>
  );
}
