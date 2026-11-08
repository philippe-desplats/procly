import type { ReactNode } from "react";

/**
 * The line a screen opens with: its title, and what the screen says about itself at the other end, such as when the
 * last answer was read and the button that asks for another. The title keeps the width of its words: when the line is
 * too narrow for everything, the actions and the controls go down to a line of their own rather than cutting it.
 */
export function TitleBar({
  title,
  tooltip,
  beside,
  under,
  actions,
  className,
  children,
}: {
  title: string;
  /** The tooltip of the title, for a title that has more names than the one shown. */
  tooltip?: string;
  /** What follows the title on its line, quietly: the mark of a removed Worktree, the legend of the Main worktree. */
  beside?: ReactNode;
  /** A quiet line under the title, cut with an ellipsis, such as the path of a Worktree. */
  under?: string;
  /** The buttons that act on what the screen shows, between the title and the controls. */
  actions?: ReactNode;
  className?: string;
  /** The controls at the end of the line: when the answer was read, and Refresh. */
  children?: ReactNode;
}) {
  return (
    <header className={className === undefined ? "title-bar" : `title-bar ${className}`}>
      <div className="title-block" title={tooltip}>
        <div className="title-line">
          <h2 className="title">{title}</h2>
          {beside}
        </div>
        {under === undefined ? null : <p className="title-under">{under}</p>}
      </div>
      {actions === undefined ? null : <div className="title-actions">{actions}</div>}
      {children === undefined ? null : <div className="title-controls">{children}</div>}
    </header>
  );
}
