// Pure grouping for the sidebar task list (tasks are sessions, projects are
// workspaces). No React here so `node --test` can exercise it.
import type { Session, Workspace } from "../services/ipc/generated/core";
import { hostTime } from "./clock.ts";
import { DAY_MS, startOfLocalDay } from "./time.ts";

export type TaskListMode = "project" | "session" | "recent" | "unread";

export const TASK_LIST_MODES: { mode: TaskListMode; label: string }[] = [
  { mode: "project", label: "By Project" },
  { mode: "session", label: "By Session" },
  { mode: "recent", label: "Recently Updated" },
  { mode: "unread", label: "By Unread" },
];

export interface TaskSection {
  key: string;
  /** Null for the flat "By Session" list. */
  label: string | null;
  /** Set when the section is a project (its header has project actions). */
  workspace?: Workspace;
  sessions: Session[];
}

/** The session's latest activity, on the Mac's clock (see lib/clock.ts). */
export function lastActivity(session: Session): number {
  return hostTime(Math.max(session.updatedAt, session.lastStartedAt ?? 0, session.createdAt));
}


/** "now", "5m", "3h", "1 day", "3 days", "2 wk", "4 mo". */
export function shortAge(at: number, now: number = Date.now()): string {
  const minutes = Math.floor(Math.max(0, now - at) / 60_000);
  if (minutes < 1) return "now";
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h`;
  const days = Math.floor(hours / 24);
  if (days < 14) return days === 1 ? "1 day" : `${days} days`;
  if (days < 60) return `${Math.floor(days / 7)} wk`;
  return `${Math.floor(days / 30)} mo`;
}

/**
 * Sections for `mode`. Archived tasks and projects are hidden unless
 * `showArchived`; `query` matches task and project names.
 */
export function taskSections(
  mode: TaskListMode,
  sessions: Session[],
  workspaces: Workspace[],
  options: { query?: string; showArchived?: boolean; unread?: Set<string>; now?: number } = {},
): TaskSection[] {
  const { query = "", showArchived = false, unread = new Set<string>(), now = Date.now() } = options;
  const needle = query.trim().toLowerCase();
  const byId = new Map(workspaces.map((w) => [w.id, w]));
  const visibleWorkspace = (w: Workspace | undefined) => w != null && (showArchived || !w.archived);
  const matches = (s: Session) =>
    needle === "" || [s.name, byId.get(s.workspaceId)?.name ?? ""].some((t) => t.toLowerCase().includes(needle));
  const tasks = sessions
    .filter((s) => (showArchived || !s.archived) && visibleWorkspace(byId.get(s.workspaceId)) && matches(s))
    .sort((a, b) => lastActivity(b) - lastActivity(a));

  switch (mode) {
    case "project": {
      const projects = workspaces
        .filter(visibleWorkspace)
        .filter((w) => needle === "" || w.name.toLowerCase().includes(needle) || tasks.some((s) => s.workspaceId === w.id));
      const latest = (w: Workspace) =>
        Math.max(w.updatedAt, ...tasks.filter((s) => s.workspaceId === w.id).map(lastActivity));
      return projects
        .sort((a, b) => latest(b) - latest(a))
        .map((w) => ({ key: w.id, label: w.name, workspace: w, sessions: tasks.filter((s) => s.workspaceId === w.id) }));
    }
    case "session":
      return [{ key: "all", label: null, sessions: [...tasks].sort((a, b) => b.createdAt - a.createdAt) }];
    case "recent": {
      const today = startOfLocalDay(now);
      const buckets: [string, number][] = [
        ["Today", today],
        ["Yesterday", today - DAY_MS],
        ["Previous 7 days", today - 7 * DAY_MS],
        ["Previous 30 days", today - 30 * DAY_MS],
        ["Older", -Infinity],
      ];
      const sections: TaskSection[] = buckets.map(([label]) => ({ key: label, label, sessions: [] }));
      for (const task of tasks) {
        const index = buckets.findIndex(([, from]) => lastActivity(task) >= from);
        sections[index].sessions.push(task);
      }
      return sections.filter((section) => section.sessions.length > 0);
    }
    case "unread": {
      const sections: TaskSection[] = [
        { key: "unread", label: "Unread", sessions: tasks.filter((s) => unread.has(s.id)) },
        { key: "read", label: "Read", sessions: tasks.filter((s) => !unread.has(s.id)) },
      ];
      return sections.filter((section) => section.sessions.length > 0);
    }
  }
}
