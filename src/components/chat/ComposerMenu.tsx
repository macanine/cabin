import { Loader } from "@mantine/core";
import { IconFile, IconSparkles, IconTerminal2 } from "@tabler/icons-react";
import { useEffect, useRef } from "react";
import type { AgentCommand } from "../../services/ipc/generated/core";
import type { MenuTrigger } from "../../lib/composerMenu";

/** The `/` and `$` list above the composer. Keyboard focus stays in the
 * textarea; the composer forwards ↑ ↓ Enter Tab Esc here. */
export function ComposerMenu({
  trigger,
  items,
  active,
  loading,
  error,
  onHover,
  onPick,
}: {
  trigger: MenuTrigger;
  items: AgentCommand[];
  active: number;
  loading: boolean;
  error: string | null;
  onHover: (index: number) => void;
  onPick: (item: AgentCommand) => void;
}) {
  const list = useRef<HTMLDivElement>(null);
  useEffect(() => {
    list.current?.querySelector<HTMLElement>(`[data-index="${active}"]`)?.scrollIntoView({ block: "nearest" });
  }, [active]);
  const title = trigger.sigil === "/" ? "Commands" : trigger.sigil === "$" ? "Skills" : "Files";
  return (
    <div className="glass-strong absolute right-0 bottom-[calc(100%+8px)] left-0 z-20 flex max-h-80 animate-enter flex-col overflow-hidden rounded-2xl" role="listbox" aria-label={title}>
      <div className="flex h-8 items-center justify-between border-b border-line/70 pr-2 pl-3.5 text-[11.5px] font-medium text-faint">
        <span>{title}</span>
        {loading && items.length === 0 && <Loader size={12} />}
      </div>
      <div className="overflow-y-auto p-1" ref={list}>
        {items.map((item, index) => {
          const Icon = item.kind === "skill" || item.insert.startsWith("$") ? IconSparkles : item.kind === "file" ? IconFile : IconTerminal2;
          return (
            <button
              key={`${item.kind}:${item.name}`}
              type="button"
              role="option"
              aria-selected={index === active}
              data-index={index}
              className={`grid min-h-8 w-full cursor-pointer grid-cols-[14px_auto_minmax(0,1fr)_auto] items-center gap-2.5 rounded-lg border-0 px-2.5 py-1.5 text-left text-[13px] text-fg transition-colors duration-100 ${index === active ? "bg-accent-soft" : "bg-transparent"}`}
              onMouseDown={(e) => e.preventDefault()}
              onMouseEnter={() => onHover(index)}
              onClick={() => onPick(item)}
            >
              <Icon size={14} className={index === active ? "text-accent" : "text-muted"} />
              <span className="font-mono text-xs font-medium whitespace-nowrap">
                {/* What gets typed: `/review`, `$pdf`, `/skill:pdf`. */}
                {item.insert.trim()}
                {item.hint && <span className="font-normal text-faint"> {item.hint}</span>}
              </span>
              <span className="truncate text-xs text-muted">{item.description}</span>
              {item.source && <span className="shrink-0 rounded-full bg-hover px-1.5 py-px text-[10.5px] text-faint">{item.source}</span>}
            </button>
          );
        })}
        {!items.length && (
          <div className="px-3 py-4 text-center text-sm text-faint">
            {loading
              ? title === "Files" ? "Loading files…" : "Asking the agent…"
              : error
                ? error
                : trigger.query
                  ? `No ${title.toLowerCase()} match “${trigger.query}”`
                  : title === "Files"
                    ? "No files here"
                    : `This agent reports no ${title.toLowerCase()} here`}
          </div>
        )}
      </div>
    </div>
  );
}
