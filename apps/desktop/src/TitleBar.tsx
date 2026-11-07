import type { ReactNode } from "react";

/**
 * The line a screen opens with: its title, and what the screen says about itself at the other end, such as when the
 * last answer was read and the button that asks for another. The title and those controls share one line.
 */
export function TitleBar({ title, children }: { title: string; children?: ReactNode }) {
  return (
    <header className="title-bar">
      <h2 className="title">{title}</h2>
      {children}
    </header>
  );
}
