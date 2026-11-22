import { IconPlayerPause, IconTargetArrow, IconX } from "@tabler/icons-react";
import type { ChatGoal } from "../../services/ipc/generated/core";
import { formatCount, formatDuration } from "../../lib/format";
import { goalBudget, goalLabel, goalTone } from "../../lib/chatTasks";

/**
 * The goal the agent is pursuing (an ACP `_meta` goal extension; live state,
 * never stored). The adapter owns it: this card shows what it last published
 * and offers only the actions it advertised.
 */
export function GoalCard({ goal, onControl }: { goal: ChatGoal; onControl: (action: "pause" | "clear") => void }) {
  const tone = goalTone(goal.status);
  const budget = goalBudget(goal);
  const stats = [
    goal.tokensUsed != null ? `${formatCount(goal.tokensUsed)}${goal.tokenBudget ? ` of ${formatCount(goal.tokenBudget)}` : ""} tokens` : null,
    goal.timeUsedSeconds != null ? formatDuration(goal.timeUsedSeconds * 1000) : null,
  ].filter(Boolean);
  const pause = goal.actions.includes("pause") && goal.status === "active";
  const clear = goal.actions.includes("clear") && (goal.status === "active" || goal.status === "paused");

  const T = { live: "text-accent bg-accent-soft", ok: "text-ok bg-ok/12", bad: "text-bad bg-bad/12" }[tone as "live" | "ok" | "bad"] ?? "text-muted bg-hover";
  const act = "press inline-flex cursor-pointer items-center gap-1 rounded-md border-0 bg-transparent px-1.5 py-1 text-xs text-muted hover:bg-hover hover:text-fg";
  return (
    <section className="glass mb-2 flex items-center gap-2.5 rounded-xl px-3 py-2" aria-label="Agent goal">
      <span className={`grid size-[26px] shrink-0 place-items-center rounded-lg ${T}`} aria-hidden><IconTargetArrow size={15} /></span>
      <div className="grid min-w-0 flex-1 gap-1">
        <div className="flex min-w-0 items-center gap-2">
          <span className="min-w-0 flex-1 truncate text-[13px] font-medium" title={goal.objective}>{goal.objective}</span>
          <span className={`shrink-0 rounded-full px-2 py-px text-[11px] font-medium ${T}`}>{goalLabel(goal.status)}</span>
        </div>
        {(budget != null || stats.length > 0) && (
          <div className="flex items-center gap-2 text-[11.5px] text-faint">
            {budget != null && (
              <span className="h-1 w-[120px] shrink-0 overflow-hidden rounded-full bg-hover" title={`${Math.round(budget * 100)}% of the token budget`}>
                <span className="block h-full origin-left rounded-full bg-accent transition-transform duration-700 ease-out" style={{ transform: `scaleX(${budget})` }} />
              </span>
            )}
            {stats.length > 0 && <span className="tabular-nums">{stats.join(" · ")}</span>}
          </div>
        )}
      </div>
      {(pause || clear) && (
        <div className="flex shrink-0 gap-0.5">
          {pause && <button type="button" className={act} onClick={() => onControl("pause")} title="Pause the goal"><IconPlayerPause size={13} /> Pause</button>}
          {clear && <button type="button" className={act} onClick={() => onControl("clear")} title="Clear the goal"><IconX size={13} /> Clear</button>}
        </div>
      )}
    </section>
  );
}
