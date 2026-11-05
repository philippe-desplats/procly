import { CopyBox } from "./CopyBox";
import { problemsFirst, type DoctorState } from "./doctor";
import type { Check } from "./protocol/Check";
import type { CheckStatus } from "./protocol/CheckStatus";
import { State } from "./StateMark";
import type { StateWord } from "./state";

function statusWord(status: CheckStatus): StateWord {
  switch (status) {
    case "failed":
      return { word: "failed", tone: "fail" };
    case "warning":
      return { word: "warning", tone: "warn" };
    case "passed":
      return { word: "passed", tone: "ok" };
    case "skipped":
      return { word: "skipped", tone: "muted" };
    default: {
      // A status a newer procly may add counts as no problem, as the JSON reference says.
      const unknown: never = status;
      return { word: String(unknown), tone: "muted" };
    }
  }
}

/** The commands of a fix, one per line, each behind a Copy button that copies exactly what it shows. */
function Fix({ fix }: { fix: string }) {
  const commands = fix.split("\n").filter((command) => command.trim() !== "");
  return (
    <div className="fix">
      {commands.map((command) => (
        <CopyBox key={command} text={command} />
      ))}
    </div>
  );
}

function CheckItem({ check }: { check: Check }) {
  return (
    <li className="item check" data-check={check.name} data-status={check.status}>
      <div className="check-head">
        <State {...statusWord(check.status)} />
        <span className="item-name mono">{check.name}</span>
      </div>
      <p className="check-message">{check.message}</p>
      {check.fix === null ? null : <Fix fix={check.fix} />}
    </li>
  );
}

/** The checks, problems first, each with its message and the fix doctor names for it. */
export function CheckList({ checks }: { checks: Check[] }) {
  return (
    <ul className="list checks">
      {problemsFirst(checks).map((check) => (
        <CheckItem key={check.name} check={check} />
      ))}
    </ul>
  );
}

/**
 * What a scope shows: its checks once doctor answered, why it could not run, or that it is running. `pick` chooses the
 * checks of the document that belong on the screen.
 */
export function Checks({ state, pick }: { state: DoctorState; pick: (checks: Check[]) => Check[] }) {
  switch (state.kind) {
    case "idle":
      return null;
    case "running":
      return state.document === null ? (
        <p className="caption">Checking…</p>
      ) : (
        <CheckList checks={pick(state.document.checks)} />
      );
    case "report":
      return <CheckList checks={pick(state.document.checks)} />;
    case "failure":
      return (
        <p role="alert" className="error">
          {state.message}
        </p>
      );
    default: {
      const unhandled: never = state;
      throw new Error(`unhandled state: ${JSON.stringify(unhandled)}`);
    }
  }
}
