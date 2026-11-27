import { IconChevronRight } from "@tabler/icons-react";
import { IS_MAC } from "../lib/platform";
import { lastActivity, shortAge } from "../lib/taskList";
import { useMinute } from "../lib/useNow";
import { useActivity, useUnread } from "../state/unread";
import { useApp } from "../state/store";
import { uiActions } from "../state/ui";
import { sessionKindName } from "./common";
import { useI18n } from "../i18n";

const LIMIT = 5;
const SEARCH_KEY = IS_MAC ? "⌘K" : "Ctrl+Shift+K";

/** Where you left off: tasks that need you first, then the latest. */
export function RecentTasks() {
  const { t } = useI18n();
  const sessions = useApp((state) => state.sessions);
  const workspaces = useApp((state) => state.workspaces);
  const activity = useActivity();
  const unread = useUnread();
  const now = useMinute();
  const rank = (id: string) => (activity.get(id) === "waiting" ? 3 : unread.has(id) ? 2 : activity.get(id) === "working" ? 1 : 0);
  const open = new Map(workspaces.filter((w) => !w.archived).map((w) => [w.id, w.name]));
  const eligible = sessions
    .filter((session) => !session.archived && open.has(session.workspaceId))
    .sort((a, b) => rank(b.id) - rank(a.id) || lastActivity(b) - lastActivity(a));
  const tasks = eligible.slice(0, LIMIT);
  if (!tasks.length) return null;
  return (
    <section className="mt-9" aria-labelledby="recent-tasks">
      <div className="mb-1.5 flex items-center justify-between gap-2 px-2">
        <h2 id="recent-tasks" className="m-0 text-sm font-medium text-muted">{t("home.recentHeading")}</h2>
        {eligible.length > LIMIT && (
          <button
            type="button"
            onClick={uiActions.openPalette}
            className="press inline-flex cursor-pointer items-center gap-1.5 rounded-md border-0 bg-transparent px-1.5 py-0.5 text-xs text-faint hover:bg-hover hover:text-fg"
          >
            <span>{t("home.allTasks", { count: eligible.length })}</span>
            <kbd className="rounded border border-line bg-surface px-1 font-mono text-[10px] text-muted">{SEARCH_KEY}</kbd>
          </button>
        )}
      </div>
      <ul className="stagger m-0 grid list-none overflow-hidden rounded-[var(--card-radius)] border border-line bg-surface p-1">
        {tasks.map((session) => {
          const state = activity.get(session.id);
          const isUnread = unread.has(session.id);
          const [dot, label, pill] = state === "waiting"
            ? ["bg-warn", t("home.badge.needsYou"), "bg-warn/12 text-warn"]
            : state === "working"
              ? ["bg-ok live-dot", t("home.badge.working"), "bg-ok/12 text-ok"]
              : isUnread
                ? ["bg-accent", t("home.badge.newReply"), "bg-accent-soft text-accent"]
                : ["bg-line", "", ""];
          return (
            <li key={session.id} className="border-t border-line/55 first:border-t-0">
              <button type="button" onClick={() => uiActions.navigate({ kind: "session", id: session.id })}
                className="press group flex h-11 w-full min-w-0 cursor-pointer items-center gap-3 rounded-[10px] border-0 bg-transparent px-2.5 text-left text-fg hover:bg-hover">
                <span className={`size-[7px] shrink-0 rounded-full ${dot}`} aria-hidden />
                <span className={`min-w-0 flex-1 truncate text-md ${isUnread || state === "waiting" ? "font-medium" : ""}`}>{session.name}</span>
                {label && <span className={`shrink-0 rounded-full px-2 py-px text-[11px] font-medium ${pill}`}>{label}</span>}
                <span className="max-w-[38%] shrink-0 truncate text-xs text-faint max-sm:hidden">{open.get(session.workspaceId)} · {sessionKindName(session.agent)}</span>
                <span className="w-11 shrink-0 text-right text-xs text-faint tabular-nums">{shortAge(lastActivity(session), now)}</span>
                <IconChevronRight size={14} className="shrink-0 text-faint opacity-0 transition-opacity duration-150 group-hover:opacity-100" aria-hidden />
              </button>
            </li>
          );
        })}
      </ul>
    </section>
  );
}
