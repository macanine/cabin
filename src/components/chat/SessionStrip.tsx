import type { ChatAsyncTask, ChatPlanEntry, SubagentTask } from "../../services/ipc/generated/core";
import { AgentPlan } from "./AgentControls";
import { SubagentStrip } from "./SubagentStrip";
import { AsyncTasks } from "./AsyncTasks";

/**
 * Quiet floating utility pills above the composer card: active execution plan,
 * parallel subagents overview, and background async tasks. Completely empty in
 * ordinary turns so the composer card stays uncluttered (live thinking/working
 * lives inside the transcript itself, not on top of the composer).
 */
export function SessionStrip({
  plan,
  subagents,
  tasks,
  onStopTask,
  now,
  open,
  onOpenChange,
  onSelect,
}: {
  plan: ChatPlanEntry[];
  subagents: SubagentTask[];
  tasks: ChatAsyncTask[];
  onStopTask: (taskId: string) => Promise<boolean>;
  now: number;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onSelect: (id: string) => void;
}) {
  return (
    <div className="mb-1.5 flex flex-wrap items-center gap-1 empty:hidden">
      <AgentPlan plan={plan} />
      <SubagentStrip tasks={subagents} now={now} open={open} onOpenChange={onOpenChange} onSelect={onSelect} />
      <AsyncTasks tasks={tasks} now={now} onStop={onStopTask} />
    </div>
  );
}
