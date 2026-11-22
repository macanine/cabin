import {
  IconActivity, IconArrowUpRight, IconCopy, IconLoader2, IconPlayerStopFilled, IconSitemap, IconTerminal2, IconTool,
} from "@tabler/icons-react";
import { useState } from "react";
import type { ChatAsyncTask } from "../../services/ipc/generated/core";
import { formatCount, formatDuration } from "../../lib/format";
import { orderTasks, taskStateLabel, taskTone } from "../../lib/chatTasks";
import { clipboard } from "../../services/ipc/dialogs";
import { uiActions } from "../../state/ui";
import { PILL, SectionLabel, StripPopover } from "./strip";

function TaskIcon({ type }: { type: string }) {
  if (type === "shell" || type === "local_bash" || type === "bash") return <IconTerminal2 size={14} />;
  if (type === "workflow") return <IconSitemap size={14} />;
  if (type === "turn") return <IconTool size={14} />;
  return <IconActivity size={14} />;
}

function meta(task: ChatAsyncTask, now: number): string {
  const live = taskTone(task.state) === "live";
  return [
    live && task.lastToolName ? task.lastToolName : null,
    task.toolUses ? `${task.toolUses} tools` : null,
    task.totalTokens ? `${formatCount(task.totalTokens)} tokens` : null,
    task.durationMs ? formatDuration(task.durationMs) : live ? formatDuration(Math.max(0, now - task.startedAt)) : null,
  ].filter(Boolean).join(" · ");
}

/** Scroll the transcript to the card that started the task and flash it. */
function revealItem(itemId: string): boolean {
  const row = document.querySelector<HTMLElement>(`[data-item-id="${CSS.escape(itemId)}"]`);
  if (!row) return false;
  row.scrollIntoView({ behavior: "smooth", block: "center" });
  row.animate([{ backgroundColor: "var(--accent-soft)" }, { backgroundColor: "transparent" }], { duration: 1400, easing: "ease-out" });
  return true;
}

const TONE_CHIP: Record<string, string> = { live: "bg-accent-soft text-accent", ok: "bg-ok/12 text-ok", bad: "bg-bad/12 text-bad", idle: "bg-hover text-muted" };
const ACT = "press inline-flex cursor-pointer items-center gap-1 rounded-md border-0 bg-transparent px-1.5 py-0.5 text-xs text-muted hover:bg-hover hover:text-fg";

function TaskRow({ task, now, stopping, onStop }: { task: ChatAsyncTask; now: number; stopping: boolean; onStop: () => void }) {
  const tone = taskTone(task.state);
  const details = meta(task, now);
  return (
    <li className="flex gap-2.5 rounded-lg px-2 py-2">
      <span className={`mt-0.5 grid size-6 shrink-0 place-items-center rounded-md ${TONE_CHIP[tone] ?? TONE_CHIP.idle}`}><TaskIcon type={task.taskType} /></span>
      <div className="grid min-w-0 flex-1 gap-0.5">
        <div className="flex min-w-0 items-center gap-2">
          <strong className="min-w-0 flex-1 truncate text-[13px]" title={task.description || task.name}>{task.name}</strong>
          <span className={`shrink-0 rounded-full px-2 py-px text-xs ${TONE_CHIP[tone] ?? TONE_CHIP.idle}`}>{taskStateLabel(task.state)}</span>
        </div>
        {task.description && task.description !== task.name && <p className="m-0 text-xs text-muted">{task.description}</p>}
        {task.summary && <p className="m-0 text-xs text-muted">{task.summary}</p>}
        {details && <span className="text-[11px] text-faint tabular-nums">{details}</span>}
        <div className="-ml-1.5 flex flex-wrap gap-0.5 pt-0.5">
          {task.itemId && (
            <button type="button" className={ACT} onClick={() => {
              if (!revealItem(task.itemId!)) uiActions.toast("Unfold the reply above to see where it started", "info");
            }}><IconArrowUpRight size={12} /> Show step</button>
          )}
          {task.outputFilePath && (
            <button type="button" className={ACT} title={task.outputFilePath} onClick={() => void clipboard.write(task.outputFilePath!).then(() => uiActions.toast("Output path copied", "success"))}>
              <IconCopy size={12} /> Output path
            </button>
          )}
          {tone === "live" && task.canStop && (
            <button type="button" className={`${ACT} !text-bad hover:!bg-bad/10`} disabled={stopping} onClick={onStop}>
              {stopping ? <IconLoader2 size={12} className="animate-spin" /> : <IconPlayerStopFilled size={11} />} Stop
            </button>
          )}
        </div>
      </div>
    </li>
  );
}

/**
 * Work the agent left running outside the turn (background shells, monitors,
 * autonomous turns: ACP async tasks). One pill with a live count; its panel
 * lists what runs and what finished recently, with the summary each reported.
 */
export function AsyncTasks({ tasks, now, onStop }: {
  tasks: ChatAsyncTask[];
  now: number;
  onStop: (taskId: string) => Promise<boolean>;
}) {
  const [stopping, setStopping] = useState<ReadonlySet<string>>(new Set());
  const { active, finished } = orderTasks(tasks);
  if (!active.length && !finished.length) return null;

  async function stop(task: ChatAsyncTask) {
    if (stopping.has(task.id)) return;
    setStopping((old) => new Set(old).add(task.id));
    try { await onStop(task.id); }
    finally { setStopping((old) => { const next = new Set(old); next.delete(task.id); return next; }); }
  }

  const failed = finished.filter((task) => taskTone(task.state) === "bad").length;
  return (
    <StripPopover
      width={400}
      target={
        <button type="button" className={PILL} aria-label="Background tasks">
          {active.length ? <span className="size-1.5 rounded-full bg-accent" aria-hidden /> : <IconActivity size={13} />}
          {active.length ? `${active.length} in background` : `${finished.length} background ${finished.length === 1 ? "task" : "tasks"}`}
          {failed > 0 && <span className="font-medium text-bad">{failed} failed</span>}
        </button>
      }
    >
      {active.length > 0 && (<><SectionLabel>Running</SectionLabel><ul className="m-0 grid list-none p-0">{active.map((task) => <TaskRow key={task.id} task={task} now={now} stopping={stopping.has(task.id)} onStop={() => void stop(task)} />)}</ul></>)}
      {finished.length > 0 && (<><SectionLabel>Recently finished</SectionLabel><ul className="m-0 grid list-none p-0">{finished.map((task) => <TaskRow key={task.id} task={task} now={now} stopping={false} onStop={() => undefined} />)}</ul></>)}
    </StripPopover>
  );
}
