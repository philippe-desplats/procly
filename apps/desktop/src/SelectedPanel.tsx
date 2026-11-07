import { Fragment, memo, useLayoutEffect, useRef, useState } from "react";

import { CopyBox } from "./CopyBox";
import type { FailureKey } from "./daemon";
import {
  composeStatement,
  foreignStatement,
  jobStatement,
  serviceStatement,
  type Statement,
} from "./inspect";
import { worktreeLabel, worktreeNames } from "./labels";
import { isSeen, useFailed } from "./needs-you";
import type { ForeignProcess } from "./protocol/ForeignProcess";
import type { WorktreeStatus } from "./protocol/WorktreeStatus";
import { useOutput, type OutputTarget, type OutputView } from "./output";
import { PortPill, PortPills } from "./PortPill";
import type { Followed, Selected } from "./selection";
import { ServiceActions } from "./ServiceActions";
import { failureOf, openLocalhost } from "./settings";
import { inWorktree } from "./shell";
import { State } from "./StateMark";
import { formatMoment } from "./time";
import type { Line } from "./transcript";

/** What to follow or read for the selection. A Service or a Job is followed, a Compose service is read once. */
function targetOf(selected: Followed): OutputTarget {
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

/**
 * The key of the failure the selection may be, nothing for a Compose service, which is listed because it runs, nor for
 * a Foreign process, which is not an ended failure.
 */
export function failureKeyOf(selected: Selected): FailureKey | null {
  switch (selected.kind) {
    case "service":
      return {
        kind: "service",
        worktree: selected.worktree.path,
        service: selected.service.service,
        started_at: selected.service.started_at,
      };
    case "job":
      return { kind: "job", id: selected.job.id };
    case "compose":
    case "foreign":
      return null;
    default: {
      const unhandled: never = selected;
      throw new Error(`unhandled selection: ${JSON.stringify(unhandled)}`);
    }
  }
}

function statementOf(selected: Followed, failed: boolean): Statement {
  switch (selected.kind) {
    case "service":
      return serviceStatement(selected.service, failed);
    case "job":
      return jobStatement(selected.job, failed);
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
function Actions({ selected, onChanged }: { selected: Followed; onChanged: () => void }) {
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
  seen,
  onMarkSeen,
  onChanged,
}: {
  selected: Selected;
  at: number;
  /** The failures the developer marked as Seen, which offer no mark any more. */
  seen: FailureKey[];
  onMarkSeen: (key: FailureKey) => void;
  onChanged: () => void;
}) {
  // A Foreign process has no output to follow and no action, and the follow is a hook, so it has a panel of its own.
  if (selected.kind === "foreign") {
    return <ForeignPanel worktree={selected.worktree} process={selected.process} />;
  }
  return (
    <FollowedPanel selected={selected} at={at} seen={seen} onMarkSeen={onMarkSeen} onChanged={onChanged} />
  );
}

/** The inspector on a Service, a Job or a Compose service, whose output it follows. */
function FollowedPanel({
  selected,
  at,
  seen,
  onMarkSeen,
  onChanged,
}: {
  selected: Followed;
  at: number;
  seen: FailureKey[];
  onMarkSeen: (key: FailureKey) => void;
  onChanged: () => void;
}) {
  const key = failureKeyOf(selected);
  const failed = useFailed(key);
  const statement = statementOf(selected, failed);
  const output = useOutput(targetOf(selected));
  return (
    <section aria-label="Selection" className="inspector-panel">
      <Heading selected={selected} />
      <div className="state-line">
        <State {...statement.state} />
        {key !== null && failed && !isSeen(seen, key) ? (
          <button
            type="button"
            className="button"
            onClick={() => {
              onMarkSeen(key);
            }}
          >
            Mark as Seen
          </button>
        ) : null}
      </div>
      <p>{statement.summary}</p>
      <p>
        <span className="caption">Next</span> {statement.next}
      </p>
      <Actions selected={selected} onChanged={onChanged} />
      {selected.kind === "job" ? (
        <>
          <p className="caption">Wait for it from a terminal</p>
          <CopyBox text={`procly wait ${selected.job.id}`} />
        </>
      ) : null}
      <Facts selected={selected} at={at} />
      <OutputPane selected={selected} output={output} />
    </section>
  );
}

function Heading({ selected }: { selected: Followed }) {
  switch (selected.kind) {
    case "service":
      return (
        <>
          <h2 className="title">{selected.service.service}</h2>
          <p className="caption foreign-where">{`Process service in ${selected.repository}`}</p>
        </>
      );
    case "job":
      return (
        <>
          <h2 className="title mono">{selected.job.command}</h2>
          <p className="caption foreign-where">
            {selected.worktree === null
              ? "Job outside Repositories"
              : `Job in ${worktreeLabel(selected.worktree)}`}
          </p>
        </>
      );
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

/**
 * The inspector on a Foreign process: what it is, the port and the pid, and the command to stop it, which the
 * developer runs. Procly never stops a process it did not start, so there is no Stop. A container has no pid, so it
 * has no command.
 */
function ForeignPanel({ worktree, process }: { worktree: WorktreeStatus; process: ForeignProcess }) {
  const statement = foreignStatement();
  return (
    <section aria-label="Selection">
      <header className="inspector-head">
        <h2 className="title">{process.kind === "process" ? process.command : process.name}</h2>
        <p className="caption foreign-where">{`Foreign process in ${worktreeLabel(worktree)}`}</p>
      </header>
      <div className="state-line">
        <State {...statement.state} />
      </div>
      <dl className="facts facts-grid">
        {process.ports.length === 0 ? null : (
          <>
            <dt>Port</dt>
            <dd>
              <PortPills ports={process.ports} />
            </dd>
          </>
        )}
        {process.kind === "process" ? (
          <>
            <dt>Process</dt>
            <dd className="facts-line" title={`${process.command}, pid ${String(process.pid)}`}>
              <code className="mono">{process.command}</code>
              {`, pid ${String(process.pid)}`}
            </dd>
          </>
        ) : (
          <>
            <dt>Compose</dt>
            <dd>
              {"project "}
              <code className="mono">{process.project}</code>
              {", service "}
              <code className="mono">{process.service}</code>
            </dd>
          </>
        )}
      </dl>
      <p className="muted">{statement.summary}</p>
      {process.kind === "process" ? (
        <>
          <p className="caption">Stop it yourself</p>
          <CopyBox text={`kill ${String(process.pid)}`} />
        </>
      ) : null}
    </section>
  );
}

/**
 * Opens the port of a Service in the browser. `Status` carries the port only, so the address assumes `http`. A failure
 * of Rust to open it stays under the button.
 */
function OpenLocalhost({ port }: { port: number }) {
  const [error, setError] = useState<string | null>(null);
  return (
    <>
      <button
        type="button"
        className="button"
        onClick={() => {
          setError(null);
          openLocalhost(port).catch((reason: unknown) => {
            setError(failureOf(reason).message);
          });
        }}
      >
        {`Open localhost:${String(port)}`}
      </button>
      {error === null ? null : (
        <p role="alert" className="error">
          {error}
        </p>
      )}
    </>
  );
}

function Facts({ selected, at }: { selected: Followed; at: number }) {
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
              <dd className="port-line">
                <PortPill port={selected.service.port} />
                {selected.service.state === "ended" ? null : <OpenLocalhost port={selected.service.port} />}
              </dd>
            </>
          )}
          <dt className="caption">Started</dt>
          <dd>{formatMoment(selected.service.started_at, at)}</dd>
        </>
      ) : null}
      {selected.kind === "compose" && selected.container.port !== null ? (
        <>
          <dt className="caption">Port</dt>
          <dd>
            <PortPill port={selected.container.port} />
          </dd>
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

/** The text of a line, which is what Find looks in. */
function textOf(line: Line): string {
  return line.spans.map((span) => span.text).join("");
}

function OutputPane({ selected, output }: { selected: Followed; output: OutputView }) {
  const snapshot = selected.kind === "compose";
  // The field of Find is revealed by its button, and closing it drops the filter.
  const [finding, setFinding] = useState(false);
  const [query, setQuery] = useState("");
  const [wrap, setWrap] = useState(false);
  const needle = finding ? query.toLowerCase() : "";
  const lines =
    needle === "" ? output.lines : output.lines.filter((line) => textOf(line).toLowerCase().includes(needle));
  return (
    <section aria-label="Output" className="output-pane">
      <h3 className="caption">Output</h3>
      <div className="output-tools">
        {output.status === "failed" ? (
          <p role="alert" className="error output-status">
            {output.message}
          </p>
        ) : (
          <p role="status" className="caption output-status">
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
        <button
          type="button"
          className="button"
          aria-expanded={finding}
          onClick={() => {
            setFinding(!finding);
            setQuery("");
          }}
        >
          Find
        </button>
        <button
          type="button"
          className="button"
          onClick={() => {
            setWrap(!wrap);
          }}
        >
          {wrap ? "No Wrap" : "Wrap"}
        </button>
      </div>
      {output.status === "failed" && snapshot ? (
        <CopyBox text={inWorktree(selected.worktree.path, ["logs", selected.container.service])} />
      ) : null}
      {finding ? (
        <input
          type="search"
          className="output-find"
          aria-label="Find in output"
          placeholder="Find"
          autoFocus
          value={query}
          onChange={(event) => {
            setQuery(event.target.value);
          }}
        />
      ) : null}
      {needle === "" ? null : (
        <p className="caption">
          {lines.length === 0
            ? "No line holds this text."
            : `${String(lines.length)} matching ${lines.length === 1 ? "line" : "lines"}`}
        </p>
      )}
      <Lines lines={lines} wrap={wrap} />
    </section>
  );
}

/** Whether the reader sits at the end of the output, so that new lines keep it there. */
function atEnd(element: HTMLElement): boolean {
  return element.scrollHeight - element.scrollTop - element.clientHeight < 8;
}

function Lines({ lines, wrap }: { lines: Line[]; wrap: boolean }) {
  const pane = useRef<HTMLPreElement>(null);
  const following = useRef(true);
  useLayoutEffect(() => {
    const element = pane.current;
    if (element !== null && following.current) {
      element.scrollTop = element.scrollHeight;
    }
  }, [lines]);
  return (
    <div className="output-box">
      <pre
        ref={pane}
        role="log"
        aria-label="Output"
        className={wrap ? "output mono wrap" : "output mono"}
        onScroll={(event) => {
          following.current = atEnd(event.currentTarget);
        }}
      >
        {lines.map((line, index) => (
          <OutputLine key={line.id} line={line} last={index === lines.length - 1} />
        ))}
      </pre>
    </div>
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
