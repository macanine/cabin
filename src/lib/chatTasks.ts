// Background tasks, subagents and goals as the chat shows them. Everything
// here reads the guest's records (ChatAsyncTask, SubagentTask, ChatGoal); none
// of it looks at which agent produced them.
import type { ChatAsyncTask, ChatGoal, ChatItem, SubagentTask } from "../services/ipc/generated/core";

export type Tone = "live" | "ok" | "bad" | "idle";

const ACTIVE_TASK = new Set(["running", "active", "paused", "waiting"]);

export const isTaskActive = (task: Pick<ChatAsyncTask, "state">) => ACTIVE_TASK.has(task.state);

export function taskTone(state: string): Tone {
  if (ACTIVE_TASK.has(state)) return state === "paused" ? "idle" : "live";
  if (state === "completed") return "ok";
  if (state === "failed") return "bad";
  return "idle";
}

const TASK_LABEL: Record<string, string> = {
  running: "Running", active: "Running", waiting: "Waiting", paused: "Paused",
  completed: "Done", failed: "Failed", stopped: "Stopped", cancelled: "Stopped", killed: "Stopped",
};

export const taskStateLabel = (state: string) => TASK_LABEL[state] ?? state.replace(/[_-]+/g, " ");

/** Running first (oldest first, they have waited longest), then the most
 * recently finished, at most `recent` of those. */
export function orderTasks(tasks: ChatAsyncTask[], recent = 5): { active: ChatAsyncTask[]; finished: ChatAsyncTask[] } {
  const active = tasks.filter(isTaskActive).sort((a, b) => a.startedAt - b.startedAt);
  const finished = tasks.filter((task) => !isTaskActive(task)).sort((a, b) => b.updatedAt - a.updatedAt).slice(0, recent);
  return { active, finished };
}

/** The newest task started by each transcript item (tool card). */
export function tasksByItem(tasks: ChatAsyncTask[]): Map<string, ChatAsyncTask> {
  const map = new Map<string, ChatAsyncTask>();
  for (const task of tasks) {
    if (!task.itemId) continue;
    const seen = map.get(task.itemId);
    if (!seen || seen.startedAt <= task.startedAt) map.set(task.itemId, task);
  }
  return map;
}

/** How long a subagent has run (live) or ran (settled). */
export function subagentElapsed(task: Pick<SubagentTask, "state" | "startedAt" | "updatedAt">, now: number): number {
  return Math.max(0, (task.state === "active" ? now : task.updatedAt) - task.startedAt);
}

const firstLine = (text: string) => text.trim().split("\n")[0]?.trim() ?? "";

/** What a subagent is doing now (active) or ended with: its newest step. */
export function latestStep(items: ChatItem[]): string | null {
  for (let index = items.length - 1; index >= 0; index--) {
    const item = items[index];
    if (item.kind === "user_message") continue;
    const text = item.subject || (item.kind === "assistant_message" || item.kind === "reasoning" ? firstLine(item.text) : item.title);
    if (text?.trim()) return text.length > 120 ? `${text.slice(0, 119)}…` : text;
  }
  return null;
}

/** Steps a subagent took: its tool calls. */
export const subagentSteps = (items: ChatItem[]) =>
  items.filter((item) => item.kind !== "user_message" && item.kind !== "assistant_message" && item.kind !== "reasoning").length;

const GOAL_LABEL: Record<string, string> = {
  active: "Active",
  paused: "Paused",
  blocked: "Blocked",
  limited: "Limit reached",
  budget_limited: "Budget reached",
  usage_limited: "Usage limit",
  time_limited: "Time limit",
  complete: "Complete",
  completed: "Complete",
  achieved: "Complete",
  cancelled: "Cleared",
  cleared: "Cleared",
  failed: "Failed",
};

export const goalLabel = (status: string) => GOAL_LABEL[status] ?? status.replace(/[_-]+/g, " ");

export function goalTone(status: string): Tone {
  if (status === "active") return "live";
  if (status === "complete" || status === "completed" || status === "achieved") return "ok";
  if (status === "failed" || status === "blocked" || status.endsWith("limited")) return "bad";
  return "idle";
}

/** Share of the token budget spent, 0–1, or null without both numbers. */
export function goalBudget(goal: Pick<ChatGoal, "tokensUsed" | "tokenBudget">): number | null {
  if (goal.tokensUsed == null || !goal.tokenBudget) return null;
  return Math.min(1, Math.max(0, goal.tokensUsed / goal.tokenBudget));
}
