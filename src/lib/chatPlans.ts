import type {
  ChatItem,
  PlanExecutionStatus,
  PlanReview,
  PlanReviewStatus,
  SubagentTask,
} from "../services/ipc/generated/core";

const TERMINAL: ReadonlySet<PlanReviewStatus> = new Set([
  "approved",
  "changes_requested",
  "cancelled",
  "expired",
  "superseded",
]);

const OPEN: ReadonlySet<PlanReviewStatus> = new Set([
  "draft",
  "awaiting_review",
  "submitting",
  "deferred",
]);

export function isPlanTerminal(status: PlanReviewStatus): boolean {
  return TERMINAL.has(status);
}

export function isPlanOpen(status: PlanReviewStatus): boolean {
  return OPEN.has(status);
}

export function planAcceptsDecision(plan: PlanReview): boolean {
  return plan.complete && (plan.status === "awaiting_review" || plan.status === "deferred");
}

export function planKey(plan: Pick<PlanReview, "planId" | "revision">): string {
  return `${plan.planId}:${plan.revision}`;
}

/** Merge one authoritative plan snapshot without letting a late draft or open
 * event resurrect a settled revision. A newer revision supersedes older open
 * ones locally as well; the backend persists the same transition. */
export function mergePlans(current: PlanReview[], incoming: PlanReview[]): PlanReview[] {
  let next = current;
  for (const record of [...incoming].sort((a, b) => a.revision - b.revision)) {
    next = mergePlan(next, record);
  }
  return next;
}

export function mergePlan(current: PlanReview[], incoming: PlanReview): PlanReview[] {
  if (
    current.some(
      (plan) => plan.planId === incoming.planId && plan.revision > incoming.revision,
    )
  ) {
    return current;
  }
  const same = current.find(
    (plan) => plan.planId === incoming.planId && plan.revision === incoming.revision,
  );
  if (same && isPlanTerminal(same.status) && !isPlanTerminal(incoming.status)) return current;

  let changed = !same;
  const next = current.map((plan) => {
    if (plan.planId !== incoming.planId || plan.revision === incoming.revision) return plan;
    if (plan.revision > incoming.revision || !isPlanOpen(plan.status)) return plan;
    changed = true;
    return {
      ...plan,
      status: "superseded" as const,
      supersededBy: incoming.revision,
    };
  });
  const index = next.findIndex(
    (plan) => plan.planId === incoming.planId && plan.revision === incoming.revision,
  );
  if (index >= 0) {
    next[index] = incoming;
  } else {
    next.push(incoming);
  }
  return changed || index < 0 ? sortPlans(next) : current;
}

export function sortPlans(plans: PlanReview[]): PlanReview[] {
  return [...plans].sort(
    (a, b) =>
      a.createdAt - b.createdAt ||
      a.planId.localeCompare(b.planId) ||
      a.revision - b.revision,
  );
}

/** Cards stay in the history. An obsolete revision-zero draft is hidden once
 * the complete revision that replaced it exists. */
export function visiblePlans(plans: PlanReview[]): PlanReview[] {
  return sortPlans(plans).filter(
    (plan) =>
      plan.revision > 0 ||
      !plans.some((other) => other.planId === plan.planId && other.revision > plan.revision),
  );
}

/** The newest plan that may receive a decision now. Deferred plans only
 * return after the user reopened them from their card. */
export function actionablePlan(
  plans: PlanReview[],
  resumedKey: string | null,
): PlanReview | null {
  const sorted = [...plans].sort(
    (a, b) => b.createdAt - a.createdAt || b.revision - a.revision,
  );
  return (
    sorted.find(
      (plan) =>
        plan.status === "awaiting_review" ||
        plan.status === "submitting" ||
        (plan.status === "deferred" && resumedKey === planKey(plan)),
    ) ?? null
  );
}

export function hasOpenPlan(plans: PlanReview[]): boolean {
  return plans.some((plan) => isPlanOpen(plan.status) && plan.status !== "draft");
}

export const PLAN_STATUS_LABEL: Record<PlanReviewStatus, string> = {
  draft: "Draft",
  awaiting_review: "Awaiting review",
  submitting: "Submitting",
  approved: "Approved",
  changes_requested: "Changes requested",
  deferred: "Deferred",
  cancelled: "Cancelled",
  expired: "Expired",
  superseded: "Superseded",
};

export const PLAN_EXECUTION_LABEL: Record<PlanExecutionStatus, string> = {
  not_started: "Not started",
  starting: "Starting",
  running: "Running",
  completed: "Completed",
  failed: "Failed",
  unknown: "Unknown",
};

export function planSummary(content: string, limit = 280): string {
  const text = content
    .replace(/```[\s\S]*?```/g, " ")
    .replace(/[#>*_`[\]-]+/g, " ")
    .replace(/\s+/g, " ")
    .trim();
  return text.length > limit ? `${text.slice(0, limit - 1).trimEnd()}…` : text;
}

/** Child items are append/update only: a completion frame may carry the task
 * without the transcript, and must never erase what the child already said. */
function mergeSubagentItems(current: ChatItem[], incoming: ChatItem[]): ChatItem[] {
  if (!incoming.length) return current;
  if (!current.length) return incoming;
  const next = new Map(incoming.map((item) => [item.id, item]));
  const merged = current.map((item) => {
    const update = next.get(item.id);
    next.delete(item.id);
    return update && update.revision >= item.revision ? update : item;
  });
  for (const item of next.values()) merged.push(item);
  return merged.sort((a, b) => a.ordinal - b.ordinal);
}

/** Merge live subagent snapshots. The backend's updatedAt is the serial;
 * older activity cannot roll a task back to active, and an update that only
 * carries the state cannot drop the child's accumulated transcript. */
export function mergeSubagents(
  current: SubagentTask[],
  incoming: SubagentTask[],
): SubagentTask[] {
  const byId = new Map(current.map((task) => [task.id, task]));
  for (const task of incoming) {
    const old = byId.get(task.id);
    if (!old) {
      byId.set(task.id, task);
      continue;
    }
    if (task.updatedAt < old.updatedAt) continue;
    const terminal = old.state === "completed" || old.state === "failed" || old.state === "stopped";
    byId.set(task.id, {
      ...old,
      ...task,
      state: terminal && task.state === "active" ? old.state : task.state,
      label: task.label || old.label,
      subagentType: task.subagentType ?? old.subagentType,
      detail: task.detail || old.detail,
      startedAt: old.startedAt || task.startedAt,
      items: mergeSubagentItems(old.items, task.items),
      updatedAt: Math.max(old.updatedAt, task.updatedAt),
    });
  }
  return [...byId.values()].sort(
    (a, b) => a.startedAt - b.startedAt || a.id.localeCompare(b.id),
  );
}

export function subagentProgress(tasks: SubagentTask[]): {
  total: number;
  completed: number;
  active: number;
  failed: number;
  stopped: number;
} {
  return {
    total: tasks.length,
    completed: tasks.filter((task) => task.state === "completed").length,
    active: tasks.filter((task) => task.state === "active").length,
    failed: tasks.filter((task) => task.state === "failed").length,
    stopped: tasks.filter((task) => task.state === "stopped").length,
  };
}

export const SUBAGENT_STATE_LABEL: Record<SubagentTask["state"], string> = {
  active: "Active",
  completed: "Done",
  failed: "Failed",
  stopped: "Stopped",
};
