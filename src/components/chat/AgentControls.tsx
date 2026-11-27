import { Menu, Tooltip } from "@mantine/core";
import { CHIP } from "../ComposerChip";
import { PILL, StripPopover } from "./strip";
import {
  IconAdjustmentsHorizontal, IconArrowUp, IconBulb, IconCheck, IconChevronDown, IconCircle,
  IconCircleCheckFilled, IconGripVertical, IconLoader2, IconPencil, IconX,
} from "@tabler/icons-react";
import { useState } from "react";
import type { ChatConfigOption, ChatContextUse, ChatPlanEntry } from "../../services/ipc/generated/core";
import { toIpcError } from "../../services/ipc/common";
import {
  contextFill, currentLabel, groupedChoices, planProgress, type QueuedMessage,
} from "../../lib/agentControls";
import { formatTokens } from "../../lib/chatStats";
import { uiActions } from "../../state/ui";

/** One agent session option (mode, thought level, …) as a composer chip. */
export function OptionPicker({ option, disabled, onChange }: {
  option: ChatConfigOption;
  disabled?: boolean;
  onChange: (value: string) => Promise<void>;
}) {
  const [applying, setApplying] = useState(false);
  async function choose(value: string) {
    if (applying || value === option.current) return;
    setApplying(true);
    try { await onChange(value); }
    catch (error) { uiActions.toast(toIpcError(error).message, "error"); }
    finally { setApplying(false); }
  }
  if (option.kind === "boolean") {
    const on = option.current === "true";
    return (
      <Tooltip label={option.description ?? option.name} openDelay={400}>
        <button
          type="button"
          className={`${CHIP} ${on ? "!bg-accent-soft !text-accent" : ""}`}
          aria-pressed={on} disabled={disabled || applying}
          onClick={() => void choose(on ? "false" : "true")}
        >
          {option.name}
        </button>
      </Tooltip>
    );
  }
  return (
    <Menu position="top-start" withinPortal shadow="md" width={260} disabled={disabled}>
      <Menu.Target>
        <button
          type="button" className={CHIP}
          disabled={disabled || applying}
          aria-label={`${option.name}: ${currentLabel(option)}`} title={option.description ?? option.name}
        >
          <IconAdjustmentsHorizontal size={14} className="shrink-0" />
          <span className="truncate">{currentLabel(option)}</span>
          <IconChevronDown size={12} className="shrink-0 opacity-60" />
        </button>
      </Menu.Target>
      <Menu.Dropdown className="max-w-[calc(100vw-24px)]">
        <Menu.Label>{option.name}</Menu.Label>
        {groupedChoices(option).map(({ group, choices }, index) => (
          <div key={group ?? `group-${index}`}>
            {group && <Menu.Label>{group}</Menu.Label>}
            {choices.map((choice) => {
              const selected = choice.value === option.current;
              return (
                <Menu.Item
                  key={choice.value}
                  className={selected ? "!bg-accent-soft/60" : ""}
                  leftSection={selected ? <IconCheck size={14} className="text-accent" /> : <span className="inline-block size-3.5" aria-hidden />}
                  onClick={() => void choose(choice.value)}
                >
                  <span className="grid gap-px whitespace-normal">
                    <strong className={`text-[13px] ${selected ? "font-semibold" : "font-medium"}`}>{choice.name || choice.value}</strong>
                    {choice.description && <small className="text-xs leading-snug text-muted">{choice.description}</small>}
                  </span>
                </Menu.Item>
              );
            })}
          </div>
        ))}
      </Menu.Dropdown>
    </Menu>
  );
}

function EntryIcon({ entry }: { entry: ChatPlanEntry }) {
  if (entry.status === "completed") return <IconCircleCheckFilled size={15} className="shrink-0 text-ok" />;
  if (entry.status === "in_progress") return <IconLoader2 size={15} className="shrink-0 animate-spin text-accent" />;
  return <IconCircle size={15} className="shrink-0 text-faint" />;
}

/** A small ring that fills as the plan's steps finish. */
function PlanRing({ share }: { share: number }) {
  const r = 6.5;
  const c = 2 * Math.PI * r;
  return (
    <svg width="16" height="16" viewBox="0 0 16 16" aria-hidden className="shrink-0 -rotate-90">
      <circle cx="8" cy="8" r={r} fill="none" strokeWidth="2" className="stroke-line" />
      <circle cx="8" cy="8" r={r} fill="none" strokeWidth="2" strokeLinecap="round" className="stroke-accent transition-[stroke-dashoffset] duration-700 ease-out" strokeDasharray={c} strokeDashoffset={c * (1 - share)} />
    </svg>
  );
}

/** The agent's execution plan (ACP `plan`): progress at a glance, the list on demand. */
export function AgentPlan({ plan }: { plan: ChatPlanEntry[] }) {
  if (!plan.length) return null;
  const progress = planProgress(plan);
  const share = progress.total ? progress.done / progress.total : 0;
  return (
    <StripPopover
      width={380}
      target={
        <button type="button" className={PILL}>
          <PlanRing share={share} />
          <span className="font-medium text-fg tabular-nums">{progress.done}/{progress.total}</span>
          <span className="max-w-[180px] truncate max-sm:hidden">{progress.current ? progress.current.content : "All steps done"}</span>
        </button>
      }
    >
      <ol className="m-0 grid list-none gap-0.5 p-0">
        {plan.map((entry, index) => (
          <li key={`${index}:${entry.content}`} className={`flex items-start gap-2.5 rounded-lg px-2 py-1.5 text-[13px] ${entry.status === "in_progress" ? "bg-accent-soft" : ""} ${entry.status === "completed" ? "text-muted" : ""}`}>
            <span className="mt-0.5"><EntryIcon entry={entry} /></span>
            <span className={`min-w-0 flex-1 ${entry.status === "completed" ? "line-through decoration-faint/60" : ""}`}>{entry.content}</span>
            {entry.priority === "high" && <span className="shrink-0 rounded-full bg-warn/12 px-1.5 py-px text-[10.5px] font-medium text-warn">High</span>}
          </li>
        ))}
      </ol>
    </StripPopover>
  );
}

/** How full the model's context window is. */
export function ContextMeter({ context }: { context: ChatContextUse | null }) {
  const fill = contextFill(context);
  if (fill == null || !context) return null;
  const percent = Math.round(fill * 100);
  const tone = fill >= 0.9 ? "text-bad" : fill >= 0.75 ? "text-warn" : "text-muted";
  const ringTone = fill >= 0.9 ? "stroke-bad" : fill >= 0.75 ? "stroke-warn" : "stroke-accent";
  const r = 5.5;
  const c = 2 * Math.PI * r;
  return (
    <Tooltip
      label={`Context ${percent}% · ${formatTokens(context.used)} of ${formatTokens(context.size)} tokens`}
      events={{ hover: true, focus: true, touch: false }}
      openDelay={250}
    >
      <span
        className={`inline-flex cursor-default items-center gap-1 rounded-md px-1.5 py-0.5 text-xs tabular-nums ${tone}`}
        role="meter"
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={percent}
        aria-label="Context window used"
        tabIndex={0}
      >
        <svg width="14" height="14" viewBox="0 0 14 14" aria-hidden className="shrink-0 -rotate-90">
          <circle cx="7" cy="7" r={r} fill="none" strokeWidth="1.8" className="stroke-line" />
          <circle
            cx="7"
            cy="7"
            r={r}
            fill="none"
            strokeWidth="1.8"
            strokeLinecap="round"
            className={`${ringTone} transition-[stroke-dashoffset] duration-500 ease-out`}
            strokeDasharray={c}
            strokeDashoffset={c * (1 - fill)}
          />
        </svg>
        {percent}%
      </span>
    </Tooltip>
  );
}

/** Follow-ups the agent suggests after a turn; a click fills the composer. */
export function Suggestions({ items, onPick }: { items: string[]; onPick: (text: string) => void }) {
  if (!items.length) return null;
  return (
    <div className="mb-2 flex flex-wrap items-center gap-1.5" role="list" aria-label="Suggested follow-ups">
      <IconBulb size={14} className="text-faint" aria-hidden />
      {items.slice(0, 4).map((item) => (
        <button key={item} type="button" role="listitem" className="press glass max-w-[260px] cursor-pointer rounded-full px-3 py-1 text-xs text-muted hover:text-fg" title={item} onClick={() => onPick(item)}>
          <span className="block truncate">{item}</span>
        </button>
      ))}
    </div>
  );
}

/** Messages typed while the agent works, sent in order when it is idle.
 * Drag to reorder, edit one back into the composer, steer it now, or remove. */
export function QueuedMessages({ queue, canSteer, failed, onRetry, onSteer, onEdit, onRemove, onMove }: {
  queue: QueuedMessage[];
  canSteer: boolean;
  failed: string[];
  onRetry?: (message: QueuedMessage) => void;
  onSteer: (message: QueuedMessage) => void;
  onEdit: (message: QueuedMessage) => void;
  onRemove: (message: QueuedMessage) => void;
  onMove: (from: number, to: number) => void;
}) {
  const [drag, setDrag] = useState<number | null>(null);
  if (!queue.length) return null;
  const act = "press grid size-6 cursor-pointer place-items-center rounded-md border-0 bg-transparent p-0 text-faint hover:bg-hover hover:text-fg";
  return (
    <ul className="m-0 mb-2 grid list-none gap-1 p-0" aria-label="Queued messages">
      {queue.map((message, index) => (
        <li
          key={message.id}
          className={`group/queue glass flex items-center gap-2 rounded-xl py-1 pr-1.5 pl-2 text-[13px] transition-opacity ${drag === index ? "opacity-50" : ""}`}
          draggable
          onDragStart={(event) => { setDrag(index); event.dataTransfer.effectAllowed = "move"; }}
          onDragEnd={() => setDrag(null)}
          onDragOver={(event) => { if (drag == null) return; event.preventDefault(); event.dataTransfer.dropEffect = "move"; }}
          onDrop={(event) => { event.preventDefault(); if (drag != null && drag !== index) onMove(drag, index); setDrag(null); }}
        >
          <span className="cursor-grab text-faint" aria-hidden><IconGripVertical size={12} /></span>
          <span className="grid size-[18px] shrink-0 place-items-center rounded-full bg-hover text-[11px] text-muted tabular-nums">{index + 1}</span>
          <span className="min-w-0 flex-1 truncate" title={message.text}>{message.text}</span>
          {failed.includes(message.id) && (
            <button type="button" className="press rounded-md border-0 bg-hover px-2 py-0.5 text-muted disabled:opacity-50"
              disabled={!onRetry} onClick={() => onRetry?.(message)} aria-label="Retry failed message">Retry</button>
          )}
          <Tooltip label="Edit in the composer" openDelay={300}><button type="button" className={act} aria-label="Edit" onClick={() => onEdit(message)}><IconPencil size={13} /></button></Tooltip>
          {canSteer && <Tooltip label="Send now, into the running turn" openDelay={300}><button type="button" className={act} aria-label="Send now" onClick={() => onSteer(message)}><IconArrowUp size={13} /></button></Tooltip>}
          <Tooltip label="Remove from the queue" openDelay={300}><button type="button" className={act} aria-label="Remove" onClick={() => onRemove(message)}><IconX size={13} /></button></Tooltip>
        </li>
      ))}
    </ul>
  );
}
