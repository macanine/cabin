import {
  IconAlertCircle, IconCheck, IconChevronRight, IconClock, IconRobot,
} from "@tabler/icons-react";
import { useState, type ReactNode } from "react";
import type { ChatItem, SubagentTask } from "../../services/ipc/generated/core";
import {
  elapsedLabel, isToolItem, type ActiveTurn, type TimelineRow,
} from "../../lib/chatTimeline";
import { latestStep, subagentElapsed, subagentSteps } from "../../lib/chatTasks";
import { formatTokens, tokensPerSecond } from "../../lib/chatStats";
import { useChat } from "./context";
import { FileChanges } from "./FileChanges";
import { Markdown } from "./Markdown";
import { ActivitySteps } from "./ActivityRows";
import { TurnHeader } from "./ActivityRow";
import { LiveLine, SweepText, WaitDots } from "./LiveLine";
import { CopyMenu, EditButton } from "./RowControls";
import { Reveal } from "../motion";

/** A completed turn's muted figure line: duration, steps, tokens, speed. */
function TurnMeta({ row }: { row: Extract<TimelineRow, { kind: "turn" }> }) {
  const { usage, tools } = useChat();
  const turnUsage = usage.get(row.turn);
  const toolCount = tools.get(row.turn) ?? 0;
  const speed = turnUsage ? tokensPerSecond(turnUsage) : null;
  const answer = row.items.find((item) => item.id === row.answerId);
  const parts = [
    row.elapsed != null && row.elapsed > 0 ? elapsedLabel(row.elapsed) : null,
    toolCount ? `${toolCount} ${toolCount === 1 ? "step" : "steps"}` : null,
    turnUsage && turnUsage.output > 0 ? `${formatTokens(turnUsage.input + turnUsage.output)} tokens` : null,
    speed != null ? `${speed < 10 ? speed.toFixed(1) : Math.round(speed)} tok/s` : null,
  ].filter(Boolean);
  return (
    <div className="group/meta flex min-h-6 items-center justify-between gap-3 text-xs text-faint">
      {parts.length > 0 && <span className="tabular-nums">{parts.join(" · ")}</span>}
      <span className="ml-auto flex gap-px opacity-0 transition-opacity duration-200 group-hover/meta:opacity-100 focus-within:opacity-100">
        <EditButton turn={row.turn} />
        {answer?.text && <CopyMenu markdown={answer.text} />}
      </span>
    </div>
  );
}

/** Inline Orca/Claude-style subagent rows inside the turn that spawned them. */
function TurnSubagents({ tasks }: { tasks: SubagentTask[] }) {
  const { now, openSubagent } = useChat();
  const activeCount = tasks.filter((task) => task.state === "active").length;
  const failedCount = tasks.filter((task) => task.state === "failed").length;
  const [open, setOpen] = useState(true);
  if (!tasks.length) return null;

  const renderRow = (task: SubagentTask) => {
    const active = task.state === "active";
    const step = latestStep(task.items);
    const steps = subagentSteps(task.items);
    const elapsed = elapsedLabel(subagentElapsed(task, now));
    return (
      <button
        key={task.id}
        type="button"
        onClick={() => openSubagent(task.id)}
        title={task.detail || task.label}
        className="press flex w-full min-w-0 cursor-pointer items-center gap-2.5 border-0 bg-transparent px-3 py-2 text-left text-fg hover:bg-hover/80"
      >
        <span className="grid size-5 shrink-0 place-items-center">
          {active ? (
            <WaitDots />
          ) : task.state === "completed" ? (
            <span className="grid size-5 place-items-center rounded-md bg-ok/12 text-ok"><IconCheck size={12} /></span>
          ) : task.state === "failed" ? (
            <span className="grid size-5 place-items-center rounded-md bg-bad/12 text-bad"><IconAlertCircle size={12} /></span>
          ) : (
            <span className="grid size-5 place-items-center rounded-md bg-hover text-muted">
              {task.state === "stopped" ? <IconClock size={12} /> : <IconRobot size={12} />}
            </span>
          )}
        </span>
        <span className="grid min-w-0 flex-1 gap-0.5">
          <span className="flex min-w-0 items-center gap-1.5">
            {task.subagentType && (
              <span className="shrink-0 rounded bg-hover px-1.5 py-px font-mono text-[10.5px] text-muted">
                {task.subagentType}
              </span>
            )}
            <span className="truncate text-[12.5px] font-medium text-fg">{task.label}</span>
          </span>
          {step && (
            <SweepText
              text={step}
              active={active}
              className="text-xs text-muted"
            />
          )}
        </span>
        <span className="flex shrink-0 items-center gap-2 text-xs text-faint tabular-nums">
          {steps > 0 && <span>{steps} {steps === 1 ? "step" : "steps"}</span>}
          <span className={active ? "text-accent" : task.state === "failed" ? "text-bad" : ""}>{elapsed}</span>
          <IconChevronRight size={13} className="text-faint" aria-hidden />
        </span>
      </button>
    );
  };

  if (tasks.length === 1) {
    return <div className="chat-process-card overflow-hidden">{renderRow(tasks[0])}</div>;
  }

  const earliest = tasks.reduce((min, task) => (task.startedAt > 0 && task.startedAt < min ? task.startedAt : min), Number.POSITIVE_INFINITY);
  const latest = tasks.reduce((max, task) => Math.max(max, task.updatedAt), 0);
  const groupElapsed = Number.isFinite(earliest)
    ? elapsedLabel(Math.max(0, (activeCount > 0 ? now : latest) - earliest))
    : null;

  return (
    <div className="chat-process-card overflow-hidden">
      <button
        type="button"
        aria-expanded={open}
        onClick={() => setOpen((value) => !value)}
        className="press flex w-full min-w-0 cursor-pointer items-center gap-2 border-0 bg-transparent px-3 py-2 text-left text-xs text-muted hover:bg-hover/70 hover:text-fg"
      >
        <span className="grid size-5 shrink-0 place-items-center">
          {activeCount > 0 ? <WaitDots /> : <IconRobot size={14} className="text-muted" />}
        </span>
        <SweepText
          text={`${tasks.length} subagents`}
          active={activeCount > 0}
          className="font-medium text-fg"
        />
        <span className="truncate text-faint tabular-nums">
          {activeCount > 0 ? `${activeCount} running` : `${tasks.length - failedCount} done`}
          {failedCount > 0 ? ` · ${failedCount} failed` : ""}
          {groupElapsed ? ` · ${groupElapsed}` : ""}
        </span>
        <IconChevronRight
          size={13}
          className={`ml-auto shrink-0 text-faint transition-transform duration-[var(--motion-reveal)] ease-[var(--ease-out)] ${open ? "rotate-90" : ""}`}
          aria-hidden
        />
      </button>
      <Reveal open={open}>
        <div className="divide-y divide-line/60 border-t border-line/60">
          {tasks.map(renderRow)}
        </div>
      </Reveal>
    </div>
  );
}

function itemRunning(items: ChatItem[]): boolean {
  return items.some((item) =>
    item.status === "running" || item.status === "pending" || item.status === "waiting",
  );
}

/**
 * One assistant reply (aligned with dsh-claude-style):
 * - Thinking and tool steps live in one collapsible process card above the answer;
 *   it stays open while steps are actively executing, then auto-folds as soon as
 *   the final answer streams/settles (unless the reader toggled it).
 * - Delegated subagents surface as crisp inline cards directly in the turn.
 * - While the turn is live, the Claude-style waiting/working row (`LiveLine`) sits
 *   right at the tail of the turn inside the transcript — never floating on top
 *   of the composer input box.
 */
export function TurnBlock({
  row,
  waiting,
  foldEpoch,
  liveTurn,
  turnSubagents = [],
}: {
  row: Extract<TimelineRow, { kind: "turn" }>;
  waiting: boolean;
  /** Bumped by every user send: folds whatever was opened above it. */
  foldEpoch: number;
  /** Active turn metadata when this row is the live running turn. */
  liveTurn?: ActiveTurn | null;
  /** Subagent tasks attributed to this turn (by `taskId` or timestamp window). */
  turnSubagents?: SubagentTask[];
}) {
  const { now } = useChat();
  const answer = row.answerId
    ? row.items.find((item) => item.id === row.answerId) ?? null
    : null;
  const progress = row.items.filter((item) => item.id !== row.answerId);
  const hasAnswer = Boolean(answer?.text.trim());

  const processWorking = row.current && !waiting && itemRunning(progress);
  const working = row.current && !waiting && (itemRunning(row.items) || Boolean(liveTurn));

  // A settled reply with no answer left to keep stays open: folding it would
  // leave a lone capsule, hiding the only content it has.
  const foldable = progress.length > 0 && (row.current || hasAnswer);
  // Per dsh-claude-style (process-fold.ts): auto-fold the process group once the
  // final answer is streaming/present and no step is still running, unless the
  // reader explicitly toggled it in this epoch.
  const autoOpen = row.current && (!hasAnswer || processWorking);
  const [manual, setManual] = useState<{ epoch: number; open: boolean } | null>(null);
  const open = foldable
    ? (manual?.epoch === foldEpoch ? manual.open : autoOpen)
    : true;

  const hasReasoning = progress.some((item) => item.kind === "reasoning");
  const reasoningRunning = progress.some((item) => item.kind === "reasoning" && item.status === "running");
  const toolSteps = progress.filter((item) => isToolItem(item)).length;
  const stepCount = progress.filter((item) => isToolItem(item) || item.kind === "reasoning").length;
  const failures = progress.filter((item) => item.status === "failed" || item.kind === "error").length;

  const headerTitle = processWorking
    ? (reasoningRunning ? "Thinking" : "Working")
    : (hasReasoning && toolSteps === 0 ? "Thought" : hasReasoning ? "Thought & steps" : "Activity");

  const meta: ReactNode[] = [
    toolSteps > 0
      ? `${toolSteps} ${toolSteps === 1 ? "step" : "steps"}`
      : (!hasReasoning && stepCount > 0 ? `${stepCount} ${stepCount === 1 ? "step" : "steps"}` : null),
    failures > 0 ? <span className="text-bad">{failures} failed</span> : null,
  ].filter((value) => value != null);

  return (
    <div className="grid min-w-0 gap-2.5">
      {foldable && (
        <TurnHeader
          title={headerTitle}
          meta={meta}
          open={open}
          working={processWorking}
          onToggle={() => setManual({ epoch: foldEpoch, open: !open })}
        />
      )}
      {progress.length > 0 && (
        <Reveal open={foldable ? open : true}>
          <div className={`chat-process-card ${row.current ? "is-live" : ""}`}>
            <ol className="m-0 grid list-none gap-0.5 px-2.5 py-2">
              <ActivitySteps items={progress} current={row.current} />
            </ol>
          </div>
        </Reveal>
      )}
      {turnSubagents.length > 0 && <TurnSubagents tasks={turnSubagents} />}
      {answer?.text && (
        <div className="turn-answer markdown min-w-0">
          <Markdown text={answer.text} streaming={working} />
        </div>
      )}
      {row.changes.length > 0 && <FileChanges changes={row.changes} />}
      {row.current && liveTurn && <LiveLine turn={liveTurn} now={now} />}
      {!row.current && <TurnMeta row={row} />}
    </div>
  );
}
