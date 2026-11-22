import { IconRobot, IconX } from "@tabler/icons-react";
import { useMemo, useRef } from "react";
import type { SubagentTask } from "../../services/ipc/generated/core";
import { SUBAGENT_STATE_LABEL } from "../../lib/chatPlans";
import { deriveTimeline, elapsedLabel } from "../../lib/chatTimeline";
import { subagentElapsed, subagentSteps } from "../../lib/chatTasks";
import { useNow } from "../../lib/useNow";
import { guestNow } from "../../lib/clock";
import { ChatContext } from "./context";
import { SelectionBubble } from "./SelectionBubble";
import { TimelineRows } from "./Timeline";

/** The child conversation, rendered with the same transcript components as
 * the main chat. Opened from a subagent row; read-only apart from copying. */
export function SubagentConversation({
  task,
  onQuote,
  onClose,
}: {
  task: SubagentTask;
  onQuote: (text: string) => void;
  onClose: () => void;
}) {
  const live = task.state === "active";
  const now = useNow(live, guestNow);
  const scroll = useRef<HTMLDivElement>(null);
  const rows = useMemo(
    () => deriveTimeline({ items: task.items, state: live ? "running" : "ready" }),
    [task.items, live],
  );
  const context = useMemo(() => ({
    sessionId: "",
    usage: new Map(),
    tools: new Map(),
    subagents: new Map(),
    tasks: new Map(),
    openSubagent: () => undefined,
    edit: () => undefined,
    readOnly: true,
    now,
  }), [now]);

  const dot = task.state === "active" ? "bg-accent" : task.state === "failed" ? "bg-bad" : task.state === "completed" ? "bg-ok" : "bg-faint";
  return (
    <aside className="glass-strong relative z-10 flex min-h-0 w-[420px] flex-none animate-fade flex-col rounded-none border-y-0 border-r-0 max-[900px]:absolute max-[900px]:inset-y-0 max-[900px]:right-0 max-[900px]:z-[12] max-[900px]:w-[min(420px,100vw)]" aria-label="Subagent conversation">
      <header className="flex items-center gap-2.5 border-b border-line/70 px-3.5 py-3">
        <span className="grid size-7 shrink-0 place-items-center rounded-lg bg-accent-soft text-accent"><IconRobot size={16} /></span>
        <div className="grid min-w-0 flex-1 gap-px">
          <strong className="truncate text-[13px]" title={task.label}>{task.label}</strong>
          <span className="flex items-center gap-1.5 text-[11px] text-muted">
            <span className={`inline-block size-[7px] rounded-full ${dot}`} aria-hidden />
            {task.subagentType || "Subagent"} · {SUBAGENT_STATE_LABEL[task.state]} · {elapsedLabel(subagentElapsed(task, now))}
            {subagentSteps(task.items) > 0 && ` · ${subagentSteps(task.items)} steps`}
          </span>
        </div>
        <button type="button" className="press grid size-7 shrink-0 cursor-pointer place-items-center rounded-md border-0 bg-transparent p-0 text-muted hover:bg-hover hover:text-fg" aria-label="Close subagent" onClick={onClose}>
          <IconX size={16} />
        </button>
      </header>
      <div ref={scroll} className="chat-scroll min-h-0 flex-1">
        <div className="chat-timeline flex w-full flex-col px-[18px] pt-[18px] pb-6">
          {task.detail && <p className="m-0 mb-4 rounded-xl bg-hover px-3 py-2.5 text-[12.5px] leading-relaxed whitespace-pre-wrap text-muted">{task.detail}</p>}
          {rows.length > 0 ? (
            <ChatContext.Provider value={context}>
              <TimelineRows rows={rows} waiting={false} />
            </ChatContext.Provider>
          ) : (
            <p className="m-0 py-6 text-center text-xs text-faint">No transcript captured.</p>
          )}
        </div>
      </div>
      <SelectionBubble containerRef={scroll} onQuote={onQuote} />
    </aside>
  );
}
