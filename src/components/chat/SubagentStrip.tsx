import {
  IconAlertCircle, IconCheck, IconChevronRight, IconClock, IconRobot,
} from "@tabler/icons-react";
import { useMemo } from "react";
import type { SubagentTask } from "../../services/ipc/generated/core";
import { subagentProgress } from "../../lib/chatPlans";
import { elapsedLabel } from "../../lib/chatTimeline";
import { latestStep, subagentElapsed, subagentSteps } from "../../lib/chatTasks";
import { PILL, StripPopover } from "./strip";

function StateMark({ state }: { state: SubagentTask["state"] }) {
  if (state === "active") {
    return (
      <span className="grid size-5 shrink-0 place-items-center" aria-hidden>
        <span className="live-dot size-2 rounded-full bg-accent" />
      </span>
    );
  }
  if (state === "completed") {
    return (
      <span className="grid size-5 shrink-0 place-items-center rounded-md bg-ok/12 text-ok" aria-hidden>
        <IconCheck size={12} stroke={2.4} />
      </span>
    );
  }
  if (state === "failed") {
    return (
      <span className="grid size-5 shrink-0 place-items-center rounded-md bg-bad/12 text-bad" aria-hidden>
        <IconAlertCircle size={12} stroke={2.2} />
      </span>
    );
  }
  return (
    <span className="grid size-5 shrink-0 place-items-center rounded-md bg-hover text-faint" aria-hidden>
      <IconClock size={12} stroke={2} />
    </span>
  );
}

function TaskRow({ task, now, onOpen }: { task: SubagentTask; now: number; onOpen: () => void }) {
  const step = latestStep(task.items);
  const steps = subagentSteps(task.items);
  const elapsed = elapsedLabel(subagentElapsed(task, now));
  const statusWord = task.state === "failed" ? "Failed" : task.state === "stopped" ? "Stopped" : null;
  const meta = [
    statusWord,
    steps > 0 ? `${steps} ${steps === 1 ? "step" : "steps"}` : null,
    elapsed || null,
  ].filter(Boolean).join(" · ");

  return (
    <button
      type="button"
      className="press flex w-full min-w-0 cursor-pointer items-start gap-2.5 rounded-lg border-0 bg-transparent px-2.5 py-2 text-left text-fg transition-colors hover:bg-hover"
      onClick={onOpen}
      title={task.detail || task.label}
    >
      <StateMark state={task.state} />
      <span className="grid min-w-0 flex-1 gap-0.5">
        <span className="flex min-w-0 items-center gap-2">
          <span className="flex min-w-0 flex-1 items-center gap-1.5">
            <span className="truncate text-[13px] font-medium text-fg">{task.label}</span>
            {task.subagentType && (
              <span className="shrink-0 rounded bg-hover px-1.5 py-px font-mono text-[10.5px] text-muted">
                {task.subagentType}
              </span>
            )}
          </span>
          <span className="flex shrink-0 items-center gap-1.5 text-[11.5px] text-faint tabular-nums">
            {meta && (
              <span className={task.state === "active" ? "text-accent" : task.state === "failed" ? "text-bad" : ""}>
                {meta}
              </span>
            )}
            <IconChevronRight size={13} className="text-faint/80" aria-hidden />
          </span>
        </span>
        {step && (
          <span className="truncate text-[12px] leading-snug text-muted">{step}</span>
        )}
      </span>
    </button>
  );
}

/** A pill for the turn's delegated work; a row opens the full subagent conversation. */
export function SubagentStrip({ tasks, now, open, onOpenChange, onSelect }: {
  tasks: SubagentTask[];
  now: number;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onSelect: (id: string) => void;
}) {
  const progress = useMemo(() => subagentProgress(tasks), [tasks]);
  if (!tasks.length) return null;

  const settled = progress.completed + progress.failed + progress.stopped;
  const runningSince = tasks.filter((task) => task.state === "active").reduce((earliest, task) => Math.min(earliest, task.startedAt), now);
  const headerMeta = [
    progress.active > 0 ? `${progress.active} active` : null,
    progress.failed > 0 ? `${progress.failed} failed` : null,
    `${settled}/${progress.total} done`,
  ].filter(Boolean).join(" · ");

  return (
    <StripPopover
      opened={open}
      onChange={onOpenChange}
      width={440}
      target={
        <button type="button" className={PILL} aria-expanded={open} onClick={() => onOpenChange(!open)}>
          {progress.active > 0 ? <span className="size-1.5 rounded-full bg-accent" /> : <IconRobot size={14} />}
          <span>Subagents</span>
          <span className="font-medium text-fg tabular-nums">{settled}/{progress.total}</span>
          {progress.active > 0 && <span className="text-accent tabular-nums">{progress.active} active · {elapsedLabel(Math.max(0, now - runningSince))}</span>}
          {progress.failed > 0 && <span className="inline-flex items-center gap-1 text-bad"><IconAlertCircle size={12} />{progress.failed} failed</span>}
        </button>
      }
    >
      <div role="region" aria-label="Subagent tasks">
        <header className="mb-1 flex items-center justify-between border-b border-line/60 px-2.5 pt-1 pb-2">
          <strong className="text-[13px] font-semibold text-fg">Subagents</strong>
          <span className="text-[11.5px] text-faint tabular-nums">{headerMeta}</span>
        </header>
        <div className="grid gap-0.5">
          {tasks.map((task) => (
            <TaskRow
              key={task.id}
              task={task}
              now={now}
              onOpen={() => { onOpenChange(false); onSelect(task.id); }}
            />
          ))}
        </div>
      </div>
    </StripPopover>
  );
}
