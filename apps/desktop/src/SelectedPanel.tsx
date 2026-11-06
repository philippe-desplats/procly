import { Fragment, memo, useLayoutEffect, useRef } from "react";

import { CopyBox } from "./CopyBox";
import { composeStatement, jobStatement, serviceStatement, type Statement } from "./inspect";
import { worktreeLabel, worktreeNames } from "./labels";
import type { WorktreeStatus } from "./protocol/WorktreeStatus";
import { useOutput, type OutputTarget, type OutputView } from "./output";
import type { Selected } from "./selection";
import { ServiceActions } from "./ServiceActions";
import { inWorktree } from "./shell";
import { State } from "./StateMark";
import { formatMoment } from "./time";
import type { Line } from "./transcript";

/** What to follow or read for the selection. A Service or a Job is followed, a Compose service is read once. */
function targetOf(selected: Selected): OutputTarget {
  switch (selected.kind) {
    case "service":
      return {
        mode: "follow",
        name: selected.service.service,
        cwd: selected.worktree.path,
        running: selected.service.state !== "ended",
      };
    case "job":
      return {
        mode: "follow",
        name: selected.job.id,
        cwd: selected.job.cwd,
        running: selected.job.state === "running",
      };
    case "compose":
      return { mode: "snapshot", name: selected.container.service, cwd: selected.worktree.path };
    default: {
      const unhandled: never = selected;
      throw new Error(`unhandled selection: ${JSON.stringify(unhandled)}`);
    }
  }
}

function statementOf(selected: Selected): Statement {
  switch (selected.kind) {
    case "service":
      return serviceStatement(selected.service);
    case "job":
      return jobStatement(selected.job);
    case "compose":
      return composeStatement(selected.container);
    default: {
      const unhandled: never = selected;
      throw new Error(`unhandled selection: ${JSON.stringify(unhandled)}`);
    }
  }
}

/**
 * Stop and Restart of the selection. A Job has none, and a Service a Group covers has no row of its own, so the
 * Group's row, selected, holds the actions that reach what runs.
 */
function Actions({ selected, onChanged }: { selected: Selected; onChanged: () => void }) {
  switch (selected.kind) {
    case "service":
      return (
        <ServiceActions
          name={selected.service.service}
          cwd={selected.worktree.path}
          running={selected.service.state !== "ended"}
          restartable={selected.service.restartable}
          onChanged={onChanged}
        />
      );
    case "compose":
      // The status lists the containers that run, and does not tell whether the Daemon holds their environment.
      return (
        <ServiceActions
          name={selected.container.service}
          cwd={selected.worktree.path}
          running
          restartable={false}
          onChanged={onChanged}
        />
      );
    case "job":
      return null;
    default: {
      const unhandled: never = selected;
      throw new Error(`unhandled selection: ${JSON.stringify(unhandled)}`);
    }
  }
}

/**
 * The inspector on the selection: what it is in words, what to do next, its actions, its facts, and its output. `at`
 * is when the answer was read, `onChanged` asks for a fresh answer after an action.
 */
export function SelectedPanel({
  selected,
  at,
  onChanged,
}: {
  selected: Selected;
  at: number;
  onChanged: () => void;
}) {
  const statement = statementOf(selected);
  const output = useOutput(targetOf(selected));
  return (
    <section aria-label="Selection">
      <Heading selected={selected} />
      <State {...statement.state} />
      <p>{statement.summary}</p>
      <p>
        <span className="caption">Next</span> {statement.next}
      </p>
      <Actions selected={selected} onChanged={onChanged} />
      <Facts selected={selected} at={at} />
      <OutputPane selected={selected} output={output} />
    </section>
  );
}

function Heading({ selected }: { selected: Selected }) {
  switch (selected.kind) {
    case "service":
      return <h2 className="title">{selected.service.service}</h2>;
    case "job":
      return <h2 className="title mono">{selected.job.command}</h2>;
    case "compose":
      return <h2 className="title">{selected.container.service}</h2>;
    default: {
      const unhandled: never = selected;
      throw new Error(`unhandled selection: ${JSON.stringify(unhandled)}`);
    }
  }
}

/** The branch, the folder and the slug of a Worktree, as terms of a list of facts. */
export function WorktreeNames({ worktree }: { worktree: WorktreeStatus }) {
  return (
    <>
      {worktreeNames(worktree).map(([name, value]) => (
        <Fragment key={name}>
          <dt className="caption">{name}</dt>
          <dd className={name === "Branch" ? undefined : "mono"}>{value}</dd>
        </Fragment>
      ))}
    </>
  );
}

function Facts({ selected, at }: { selected: Selected; at: number }) {
  return (
    <dl className="facts">
      {selected.worktree === null ? null : (
        <>
          <dt className="caption">Worktree</dt>
          <dd>{worktreeLabel(selected.worktree)}</dd>
          <WorktreeNames worktree={selected.worktree} />
        </>
      )}
      {selected.kind === "service" ? (
        <>
          {selected.service.port === null ? null : (
            <>
              <dt className="caption">Port</dt>
              <dd className="mono">{`:${String(selected.service.port)}`}</dd>
            </>
          )}
          <dt className="caption">Started</dt>
          <dd>{formatMoment(selected.service.started_at, at)}</dd>
        </>
      ) : null}
      {selected.kind === "compose" && selected.container.port !== null ? (
        <>
          <dt className="caption">Port</dt>
          <dd className="mono">{`:${String(selected.container.port)}`}</dd>
        </>
      ) : null}
      {selected.kind === "job" ? (
        <>
          <dt className="caption">Job</dt>
          <dd className="mono">{selected.job.id}</dd>
          <dt className="caption">Directory</dt>
          <dd className="mono">{selected.job.cwd}</dd>
        </>
      ) : null}
    </dl>
  );
}

const WORDS: Record<Exclude<OutputView["status"], "snapshot">, string> = {
  connecting: "Connecting…",
  following: "Following",
  ended: "Ended",
  paused: "Paused while the window is hidden",
  failed: "",
};

function OutputPane({ selected, output }: { selected: Selected; output: OutputView }) {
  const snapshot = selected.kind === "compose";
  return (
    <section aria-label="Output">
      <h3 className="caption">Output</h3>
      {output.status === "failed" ? (
        <>
          <p role="alert" className="error">
            {output.message}
          </p>
          {snapshot ? (
            <CopyBox text={inWorktree(selected.worktree.path, ["logs", selected.container.service])} />
          ) : null}
        </>
      ) : (
        <p role="status" className="caption">
          {output.status === "snapshot" && output.at !== null
            ? `Snapshot at ${formatMoment(output.at, output.at)}`
            : WORDS[output.status === "snapshot" ? "connecting" : output.status]}
        </p>
      )}
      {snapshot ? (
        <button type="button" className="button" onClick={output.refresh}>
          Refresh
        </button>
      ) : null}
      <Lines lines={output.lines} />
    </section>
  );
}

/** Whether the reader sits at the end of the output, so that new lines keep it there. */
function atEnd(element: HTMLElement): boolean {
  return element.scrollHeight - element.scrollTop - element.clientHeight < 8;
}

function Lines({ lines }: { lines: Line[] }) {
  const pane = useRef<HTMLPreElement>(null);
  const following = useRef(true);
  useLayoutEffect(() => {
    const element = pane.current;
    if (element !== null && following.current) {
      element.scrollTop = element.scrollHeight;
    }
  }, [lines]);
  return (
    <pre
      ref={pane}
      role="log"
      aria-label="Output"
      className="output mono"
      onScroll={(event) => {
        following.current = atEnd(event.currentTarget);
      }}
    >
      {lines.map((line, index) => (
        <OutputLine key={line.id} line={line} last={index === lines.length - 1} />
      ))}
    </pre>
  );
}

/** One line, drawn again only when it changed. The output is untrusted text and is only ever drawn as text. */
const OutputLine = memo(function OutputLine({ line, last }: { line: Line; last: boolean }) {
  return (
    <>
      <span className={line.note ? "line note" : "line"}>
        {line.spans.map((span, index) => (
          <span
            // A line is rewritten as a whole, its runs have no identity of their own.
            key={index}
            className={[span.colour === null ? "" : `fg-${span.colour}`, span.bold ? "bold" : ""]
              .filter((name) => name !== "")
              .join(" ")}
          >
            {span.text}
          </span>
        ))}
      </span>
      {last ? null : "\n"}
    </>
  );
});
