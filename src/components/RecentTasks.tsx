import { lastActivity, shortAge } from "../lib/taskList";
import { useMinute } from "../lib/useNow";
import { useActivity, useUnread } from "../state/unread";
import { useApp } from "../state/store";
import { uiActions } from "../state/ui";
import { sessionKindName } from "./common";

const LIMIT = 5;

/** Where you left off: tasks that need you first, then the latest. */
export function RecentTasks() {
  const sessions = useApp((state) => state.sessions);
  const workspaces = useApp((state) => state.workspaces);
  const activity = useActivity();
  const unread = useUnread();
  const now = useMinute();
  const rank = (id: string) => (activity.get(id) === "waiting" ? 3 : unread.has(id) ? 2 : activity.get(id) === "working" ? 1 : 0);
  const open = new Map(workspaces.filter((w) => !w.archived).map((w) => [w.id, w.name]));
  const tasks = sessions
    .filter((session) => !session.archived && open.has(session.workspaceId))
    .sort((a, b) => rank(b.id) - rank(a.id) || lastActivity(b) - lastActivity(a))
    .slice(0, LIMIT);
  if (!tasks.length) return null;
  return (
    <section className="mt-9" aria-labelledby="recent-tasks">
      <h2 id="recent-tasks" className="m-0 mb-1 px-2 text-sm font-medium text-muted">Pick up where you left off</h2>
      <ul className="stagger m-0 grid list-none p-0">
        {tasks.map((session) => {
          const state = activity.get(session.id);
          const [tone, label] = state === "waiting" ? ["bg-warn", "Needs you"] : state === "working" ? ["bg-ok live-dot", "Working"] : unread.has(session.id) ? ["bg-accent", "New reply"] : ["bg-line", ""];
          return (
            <li key={session.id} className="border-t border-line/70 first:border-t-0">
              <button type="button" onClick={() => uiActions.navigate({ kind: "session", id: session.id })}
                className="press flex h-11 w-full min-w-0 cursor-pointer items-center gap-3 rounded-lg border-0 bg-transparent px-2 text-left text-fg hover:bg-hover">
                <span className={`size-[7px] shrink-0 rounded-full ${tone}`} aria-hidden />
                <span className="min-w-0 flex-1 truncate text-md">{session.name}</span>
                {label && <span className={`shrink-0 text-xs font-medium ${state === "waiting" ? "text-warn" : "text-muted"}`}>{label}</span>}
                <span className="max-w-[40%] shrink-0 truncate text-xs text-faint max-sm:hidden">{open.get(session.workspaceId)} · {sessionKindName(session.agent)}</span>
                <span className="w-12 shrink-0 text-right text-xs text-faint tabular-nums">{shortAge(lastActivity(session), now)}</span>
              </button>
            </li>
          );
        })}
      </ul>
    </section>
  );
}
