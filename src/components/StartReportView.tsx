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
import { BootDetails, BootHero, BootStep, BootSteps, Elapsed, Ticker } from "./Boot";
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

/**
 * A machine start, told calmly: one ring with overall progress, then the
 * steps as the start reaches them. The running step shows a live line from
 * the start log. Facts and full logs wait behind
 * "Details", which opens on its own when the start fails. `children`
 * (recovery help) sits at the end of the same column, so on the start page
 * it shares the window height with the log instead of pushing past it.
 */
export function StartReportView({ report, compact = false, children }: { report: StartReport; compact?: boolean; children?: ReactNode }) {
  const running = report.state === "running";
  const failed = report.state === "failed";
  const now = useNow(running);
  const [open, toggle] = useAutoOpen(failed);
  const [tab, setTab] = useState<"log" | "console">("log");
  const current = focusPhase(report);
  const steps = visiblePhases(report);
  const remaining = remainingPhases(report);
  const ticker = running ? logTicker(report.logTail) : null;

  const title = failed ? "The machine did not start" : running ? (isAttach(report) ? "Reconnecting to your machine" : "Starting your machine") : "Your machine is ready";
  const subtitle = failed ? current?.title ?? report.error : running ? current?.title ?? "Getting ready" : "Opening your workspace";
  const path = tab === "log" ? report.logPath : report.serialPath;

  return (
    <div className={`grid min-w-0 ${compact ? "gap-3 pt-2" : "gap-5"}`}>
      {compact ? (
        <p className="m-0 text-sm text-muted [overflow-wrap:anywhere]">{formatDateTime(report.startedAt)}{report.error && ` · ${report.error}`}</p>
      ) : (
        <BootHero
          tone={failed ? "bad" : running ? "running" : "ok"}
          progress={startProgress(report)}
          title={title}
          subtitle={subtitle}
          aside={<Elapsed title={`Began ${formatDateTime(report.startedAt)}`}>{formatDuration(reportElapsed(report, now))}</Elapsed>}
        />
      )}

      <BootSteps>
        {steps.map((phase, index) => {
          const elapsed = phaseElapsed(phase, now);
          return (
            <BootStep key={phase.id} order={index} state={phase.state} title={phase.title} time={elapsed != null ? formatDuration(elapsed) : ""}>
              {phase.state === "running" && ticker && <Ticker title={ticker}>{ticker}</Ticker>}
              {phase.state === "failed" && phase.detail && <div className="text-sm text-bad [overflow-wrap:anywhere]">{phase.detail}</div>}
            </BootStep>
          );
        })}
      </BootSteps>
      {remaining > 0 && (
        <div className="text-center text-xs text-faint">{remaining} more {remaining === 1 ? "step" : "steps"}</div>
      )}

      <BootDetails open={open} onToggle={toggle}>
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
      </BootDetails>
      {children}
    </div>
  );
}
