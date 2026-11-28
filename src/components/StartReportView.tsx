import { ActionIcon, SegmentedControl, Tooltip } from "@mantine/core";
import { IconCopy } from "@tabler/icons-react";
import { type ReactNode, useState } from "react";
import type { StartReport } from "../services/ipc/generated/core";
import { clipboard } from "../services/ipc/dialogs";
import {
  focusPhase,
  isAttach,
  logTicker,
  phaseElapsed,
  remainingPhases,
  reportElapsed,
  startProgress,
  visiblePhases,
} from "../lib/startReport";
import { formatDuration } from "../lib/format";
import { formatDateTime } from "../lib/time";
import { useNow } from "../lib/useNow";
import { LogView } from "./LogView";
import { BootDetails, BootStep, Elapsed, Ticker } from "./Boot";
import { CabinWordmark } from "./CabinMark";
import { MachineStage } from "./MachineStage";
import { useAutoOpen } from "./motion";

function CopyPath({ path }: { path: string }) {
  return (
    <Tooltip label="Copy path">
      <ActionIcon size="sm" variant="subtle" color="gray" aria-label="Copy path" onClick={() => void clipboard.write(path)}>
        <IconCopy size={13} />
      </ActionIcon>
    </Tooltip>
  );
}

/** Facts and full logs of a start. */
function StartLogs({ report }: { report: StartReport }) {
  const [tab, setTab] = useState<"log" | "console">("log");
  const path = tab === "log" ? report.logPath : report.serialPath;
  return (
    <>
      {report.facts.length > 0 && (
        <dl className="m-0 grid grid-cols-[max-content_minmax(0,1fr)] gap-x-4 gap-y-1 text-sm max-sm:grid-cols-1">
          {report.facts.map((fact) => (
            <div key={fact.label} className="contents">
              <dt className="text-faint">{fact.label}</dt>
              <dd className={`m-0 [overflow-wrap:anywhere] ${fact.value.startsWith("/") ? "font-mono text-xs" : ""}`}>{fact.value}</dd>
            </div>
          ))}
        </dl>
      )}
      <div className="flex min-w-0 items-center gap-2">
        <SegmentedControl
          size="xs"
          value={tab}
          onChange={(value) => setTab(value as "log" | "console")}
          data={[{ value: "log", label: "Start log" }, { value: "console", label: "Guest console" }]}
        />
        {path && (
          <span className="flex min-w-0 flex-1 items-center gap-1 font-mono text-xs text-faint">
            <span className="truncate" title={path}>{path}</span>
            <CopyPath path={path} />
          </span>
        )}
      </div>
      {tab === "log"
        ? <LogView key="log" variant="terminal" label="Start log" lines={report.logTail} empty="Waiting for startup log output." />
        : <LogView key="console" variant="terminal" label="Guest console" lines={report.serialTail} empty="No console output from this boot yet." />}
    </>
  );
}

function StartSteps({ report, now }: { report: StartReport; now: number }) {
  const ticker = report.state === "running" ? logTicker(report.logTail) : null;
  const remaining = remainingPhases(report);
  return (
    <>
      <ol className="iw-stages" aria-label="Start steps">
        {visiblePhases(report).map((phase, index) => {
          const elapsed = phaseElapsed(phase, now);
          return (
            <BootStep key={phase.id} order={index} state={phase.state} title={phase.title} time={elapsed != null ? formatDuration(elapsed) : ""}>
              {phase.state === "running" && ticker && <Ticker title={ticker}>{ticker}</Ticker>}
              {phase.state === "failed" && phase.detail && <div className="text-sm text-bad [overflow-wrap:anywhere]">{phase.detail}</div>}
            </BootStep>
          );
        })}
      </ol>
      {remaining > 0 && <div className="pl-[42px] text-xs text-faint">{remaining} more {remaining === 1 ? "step" : "steps"}</div>}
    </>
  );
}

/** A start as a record (Settings → Machine): when, the steps, and its logs. */
export function StartReportView({ report }: { report: StartReport }) {
  const now = useNow(report.state === "running");
  const [open, toggle] = useAutoOpen(report.state === "failed");
  return (
    <div className="grid min-w-0 gap-3 pt-2">
      <p className="m-0 text-sm text-muted [overflow-wrap:anywhere]">{formatDateTime(report.startedAt)}{report.error && ` · ${report.error}`}</p>
      <StartSteps report={report} now={now} />
      <BootDetails open={open} onToggle={toggle}><StartLogs report={report} /></BootDetails>
    </div>
  );
}

/**
 * A start in progress, or one that failed. Progress and the steps begun so
 * far sit beside the machine card. After a failure the way out comes first
 * (`recovery`, and `actions` in the bar), then the steps, then the logs.
 */
export function StartStage({ report, card, recovery, actions }: {
  report: StartReport;
  card: (screen: string | null) => ReactNode;
  recovery?: ReactNode;
  actions?: ReactNode;
}) {
  const running = report.state === "running";
  const failed = report.state === "failed";
  const now = useNow(running);
  const [open, toggle] = useAutoOpen(failed);
  const current = focusPhase(report);
  const progress = startProgress(report);
  const title = failed ? "The machine did not start" : running ? (isAttach(report) ? "Reconnecting to your machine" : "Starting your machine") : "Your machine is ready";
  const subtitle = failed ? `Stopped at “${current?.title ?? "startup"}”.` : running ? current?.title ?? "Getting ready" : "Opening your workspace";
  return (
    <MachineStage
      eyebrow={<CabinWordmark size={18} animated={running} />}
      title={title}
      lead={subtitle}
      lede={<Elapsed title={`Began ${formatDateTime(report.startedAt)}`}>{formatDuration(reportElapsed(report, now))}</Elapsed>}
      main={<>
        {!failed && (
          <div className="stage-progress" role="progressbar" aria-label="Start progress" aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(progress * 100)}>
            <span style={{ inlineSize: `${Math.round(progress * 100)}%` }} />
          </div>
        )}
        {recovery}
        <StartSteps report={report} now={now} />
        <BootDetails open={open} onToggle={toggle}><StartLogs report={report} /></BootDetails>
      </>}
      aside={card(running ? logTicker(report.logTail) : failed ? `stopped: ${(current?.title ?? "startup").toLowerCase()}` : null)}
      bar={actions}
    />
  );
}
