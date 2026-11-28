import { Menu } from "@mantine/core";
import { IconCheck, IconChevronDown, IconChevronLeft } from "@tabler/icons-react";
import { useEffect, useMemo, useRef } from "react";
import type { SubagentTask } from "../../services/ipc/generated/core";
import { SUBAGENT_STATE_LABEL } from "../../lib/chatPlans";
import { activeTurn, deriveTimeline, elapsedLabel } from "../../lib/chatTimeline";
import { subagentElapsed, subagentSteps } from "../../lib/chatTasks";
import { useNow } from "../../lib/useNow";
import { guestNow } from "../../lib/clock";
import { ChatContext } from "./context";
import { Markdown } from "./Markdown";
import { SelectionBubble } from "./SelectionBubble";
import { TimelineRows } from "./Timeline";

function stateDot(state: SubagentTask["state"]): string {
  if (state === "active") return "bg-accent animate-breathe";
  if (state === "failed") return "bg-bad";
  if (state === "completed") return "bg-ok";
  return "bg-faint";
}

/**
 * Full-pane subagent view inside the chat column. Shares the exact `792px`
 * centered grid, typography, user-prompt bubble, and turn renderer as the
 * main agent conversation, with an Orca-style breadcrumb bar at the top.
 */
export function SubagentConversation({
  task,
  tasks = [],
  onSelect,
  onQuote,
  onClose,
}: {
  task: SubagentTask;
  tasks?: SubagentTask[];
  onSelect?: (id: string) => void;
  onQuote: (text: string) => void;
  onClose: () => void;
}) {
  const live = task.state === "active";
  const now = useNow(live || tasks.some((entry) => entry.state === "active"), guestNow);
  const scroll = useRef<HTMLDivElement>(null);

  useEffect(() => {
    scroll.current?.scrollTo({ top: 0 });
  }, [task.id]);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape" && !event.defaultPrevented) {
        event.preventDefault();
        onClose();
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [onClose]);

  const rows = useMemo(
    () => deriveTimeline({ items: task.items, state: live ? "running" : "ready" }),
    [task.items, live],
  );
  const liveTurn = useMemo(
    () => (live ? activeTurn(task.items, "running") : null),
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

  const steps = subagentSteps(task.items);
  const elapsed = elapsedLabel(subagentElapsed(task, now));

  return (
    <section
      aria-label={`Subagent: ${task.label}`}
      className="absolute inset-0 z-20 flex min-h-0 flex-col bg-bg animate-fade"
    >
      <header className="shrink-0 border-b border-line/60 bg-bg">
        <div className="mx-auto flex h-11 w-[min(100%,792px)] items-center gap-1.5 px-6">
          <button
            type="button"
            onClick={onClose}
            className="press -ml-2 inline-flex h-7 shrink-0 cursor-pointer items-center gap-1 rounded-lg border-0 bg-transparent px-2 text-[12.5px] font-medium text-muted hover:bg-hover hover:text-fg"
          >
            <IconChevronLeft size={14} aria-hidden />
            Main agent
          </button>
          <span className="shrink-0 text-xs text-faint" aria-hidden>/</span>
          {tasks.length > 1 && onSelect ? (
            <Menu position="bottom-start" withinPortal width={300} shadow="md">
              <Menu.Target>
                <button
                  type="button"
                  className="press inline-flex h-7 max-w-[300px] min-w-0 cursor-pointer items-center gap-1.5 rounded-lg border-0 bg-transparent px-2 text-[13px] font-medium text-fg hover:bg-hover"
                >
                  <span className={`size-1.5 shrink-0 rounded-full ${stateDot(task.state)}`} aria-hidden />
                  <span className="truncate">{task.label}</span>
                  <IconChevronDown size={13} className="shrink-0 text-faint" aria-hidden />
                </button>
              </Menu.Target>
              <Menu.Dropdown>
                <Menu.Item leftSection={<IconChevronLeft size={14} />} onClick={onClose}>
                  Main agent
                </Menu.Item>
                <Menu.Divider />
                {tasks.map((entry) => {
                  const selected = entry.id === task.id;
                  return (
                    <Menu.Item
                      key={entry.id}
                      className={selected ? "!bg-accent-soft/60" : ""}
                      leftSection={<span className={`inline-block size-2 rounded-full ${stateDot(entry.state)}`} />}
                      rightSection={selected ? <IconCheck size={13} className="text-accent" /> : <span className="text-[11px] text-faint">{SUBAGENT_STATE_LABEL[entry.state]}</span>}
                      onClick={() => onSelect(entry.id)}
                    >
                      <span className="line-clamp-2 text-[13px] whitespace-normal">{entry.label}</span>
                    </Menu.Item>
                  );
                })}
              </Menu.Dropdown>
            </Menu>
          ) : (
            <span className="inline-flex min-w-0 items-center gap-1.5 px-1.5 text-[13px] font-medium text-fg">
              <span className={`size-1.5 shrink-0 rounded-full ${stateDot(task.state)}`} aria-hidden />
              <span className="truncate" title={task.label}>{task.label}</span>
            </span>
          )}
          {task.subagentType && (
            <span className="shrink-0 rounded-md bg-hover px-1.5 py-px font-mono text-[10.5px] text-muted">
              {task.subagentType}
            </span>
          )}
          <span className="ml-auto flex shrink-0 items-center gap-1.5 text-[12px] text-faint tabular-nums">
            <span>{SUBAGENT_STATE_LABEL[task.state]}</span>
            {elapsed && (
              <>
                <span aria-hidden>·</span>
                <span>{elapsed}</span>
              </>
            )}
            {steps > 0 && (
              <>
                <span aria-hidden>·</span>
                <span>{steps} {steps === 1 ? "step" : "steps"}</span>
              </>
            )}
          </span>
        </div>
      </header>
      <div ref={scroll} className="chat-scroll min-h-0 flex-1">
        <div className="chat-timeline mx-auto flex w-[min(100%,792px)] flex-col px-6 pt-5 pb-12">
          {task.detail && (
            <div className="flex min-w-0 justify-end">
              <div className="chat-user-bubble selectable max-w-[min(88%,640px)] min-w-0 rounded-2xl rounded-br-md px-3.5 py-2 text-[14px] leading-[1.6] [overflow-wrap:anywhere]">
                <Markdown text={task.detail} compact />
              </div>
            </div>
          )}
          {rows.length > 0 ? (
            <ChatContext.Provider value={context}>
              <TimelineRows rows={rows} waiting={false} activeTurn={liveTurn} />
            </ChatContext.Provider>
          ) : (
            <div className="mt-4 flex items-center gap-2 text-[12.5px] text-faint">
              {live && <span className="size-1.5 rounded-full bg-accent animate-breathe" aria-hidden />}
              <span>{live ? "Running in background…" : "No transcript recorded for this subagent."}</span>
            </div>
          )}
        </div>
      </div>
      <SelectionBubble containerRef={scroll} onQuote={onQuote} />
    </section>
  );
}
