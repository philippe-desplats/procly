/** A port as a pill, the one drawing every screen uses, so that a port looks the same on a row and in the inspector. */
export function PortPill({ port }: { port: number }) {
  return <span className="port mono">{String(port)}</span>;
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
