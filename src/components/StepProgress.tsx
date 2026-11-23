import type { ReactNode } from "react";
import { StateIcon, type StateTone } from "./common";
import { LogView } from "./LogView";
import { DisclosureChevron, Reveal, useAutoOpen } from "./motion";

/** One step of a running or finished job, in display terms. */
export interface ProgressStepItem {
  key: string;
  title: ReactNode;
  tone: StateTone;
  /** Right-aligned state text ("Done · 12 s"). */
  status?: ReactNode;
  /** Settled (succeeded, skipped, rolled back): its log folds away. Failed
   *  steps are not "finished" here — their log is what the reader needs. */
  finished: boolean;
  log: string[];
  /** Live content under the title, e.g. a download bar. */
  extra?: ReactNode;
  /** One-line explanation under the title. */
  detail?: ReactNode;
  /** Full log file on this computer, when worth pointing to. */
  logPath?: string | null;
}

/**
 * The step list shared by every install-like flow (the machine installer,
 * Node.js versions, operations): status icon, title and state per step, plus
 * a bounded live log that collapses once the step finishes.
 */
export function StepProgress({ steps }: { steps: ProgressStepItem[] }) {
  return (
    <ol className="progress-steps">
      {steps.map((step) => (
        <li key={step.key} className="progress-step">
          <div className="row">
            <StateIcon tone={step.tone} />
            <span className="grow">{step.title}</span>
            {step.status != null && <span className="faint fs-12">{step.status}</span>}
          </div>
          {step.extra}
          {step.detail && <div className="field-reason">{step.detail}</div>}
          {step.log.length > 0 && <StepLog title={step.title} lines={step.log} finished={step.finished} />}
          {step.logPath && (
            <div className="field-reason">
              Full log: <span className="mono" style={{ userSelect: "text" }}>{step.logPath}</span>
            </div>
          )}
        </li>
      ))}
    </ol>
  );
}

/**
 * Open while the step runs (or failed), collapsed automatically once it
 * finishes. A click on the toggle overrides that either way.
 */
function StepLog({ title, lines, finished }: { title: ReactNode; lines: string[]; finished: boolean }) {
  const [open, toggle] = useAutoOpen(!finished);
  return (
    <>
      {finished && (
        <button type="button" className="log-toggle" aria-expanded={open} onClick={toggle}>
          <DisclosureChevron open={open} />
          {open ? "Hide log" : `Show log · ${lines.length} ${lines.length === 1 ? "line" : "lines"}`}
        </button>
      )}
      <Reveal open={open}>
        <LogView lines={lines} label={typeof title === "string" ? `${title} log` : undefined} />
      </Reveal>
    </>
  );
}
