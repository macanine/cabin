import {
  IconAlertCircle, IconCheck, IconChevronRight, IconClock,
  IconRobot, IconX,
} from "@tabler/icons-react";
import { useMemo } from "react";
import type { SubagentTask } from "../../services/ipc/generated/core";
import { subagentProgress } from "../../lib/chatPlans";
import { elapsedLabel } from "../../lib/chatTimeline";
import { latestStep, subagentElapsed, subagentSteps } from "../../lib/chatTasks";
import { PILL, StripPopover } from "./strip";

const STATE_CHIP: Record<SubagentTask["state"], string> = {
  active: "bg-accent-soft text-accent",
  completed: "bg-ok/12 text-ok",
  failed: "bg-bad/12 text-bad",
  stopped: "bg-hover text-muted",
};

function TaskState({ task }: { task: SubagentTask }) {
  const label = { active: "Active", completed: "Done", failed: "Failed", stopped: "Stopped" }[task.state];
  const Icon = task.state === "completed" ? IconCheck : task.state === "failed" ? IconX : task.state === "stopped" ? IconClock : null;
  return <span className={`inline-flex shrink-0 items-center gap-1 rounded-full px-2 py-px text-xs ${STATE_CHIP[task.state]}`}>{Icon && <Icon size={12} />}{label}</span>;
}

function TaskRow({ task, now, onOpen }: { task: SubagentTask; now: number; onOpen: () => void }) {
  const step = latestStep(task.items);
  const steps = subagentSteps(task.items);
  return (
    <button type="button" className="press flex w-full cursor-pointer items-center gap-2.5 rounded-lg border-0 bg-transparent px-2 py-2 text-left text-fg hover:bg-hover" onClick={onOpen} title={task.detail || task.label}>
      <span className={`size-2 shrink-0 rounded-full ${task.state === "active" ? "bg-accent" : task.state === "failed" ? "bg-bad" : task.state === "completed" ? "bg-ok" : "bg-faint"}`} aria-hidden />
      <span className="grid min-w-0 flex-1 gap-0.5">
        <span className="flex min-w-0 items-baseline gap-1.5">
          {task.subagentType && <span className="shrink-0 rounded bg-hover px-1 font-mono text-[10.5px] text-muted">{task.subagentType}</span>}
          <span className="truncate text-[13px] font-medium">{task.label}</span>
        </span>
        {step && <span className="truncate text-xs text-muted">{step}</span>}
        <span className="text-[11px] text-faint tabular-nums">{elapsedLabel(subagentElapsed(task, now))}{steps > 0 ? ` · ${steps} ${steps === 1 ? "step" : "steps"}` : ""}</span>
      </span>
      <TaskState task={task} />
      <IconChevronRight size={13} className="shrink-0 text-faint" />
    </button>
  );
}

/** A pill for the turn's delegated work; a row opens the child conversation
 *  in the side panel. */
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


  return (
    <StripPopover
      opened={open}
      onChange={onOpenChange}
      width={400}
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
        <header className="flex items-center justify-between px-2 pt-1 pb-1">
          <strong className="text-[13px]">Subagents</strong>
        </header>
        <div className="grid gap-0.5">
          {tasks.map((task) => <TaskRow key={task.id} task={task} now={now} onOpen={() => { onOpenChange(false); onSelect(task.id); }} />)}
        </div>
      </div>
    </StripPopover>
  );
}
