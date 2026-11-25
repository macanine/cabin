import { Loader } from "@mantine/core";
import { IconFile, IconSparkles, IconTerminal2 } from "@tabler/icons-react";
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import type { useComposerMenu } from "./useComposerMenu";

/** An overlay: the textarea retains focus and announces the active option. */
export function ComposerMenu({ menu, placement = "above" }: {
  menu: ReturnType<typeof useComposerMenu>;
  placement?: "above" | "below";
}) {
  const { trigger, items, index, loading, error, emptyReason, skillsOnly, pick, setIndex, listId } = menu;
  const panel = useRef<HTMLDivElement>(null);
  const list = useRef<HTMLDivElement>(null);
  const [space, setSpace] = useState<{ side: typeof placement; height: number } | null>(null);
  useLayoutEffect(() => {
    const anchor = panel.current?.offsetParent;
    if (!anchor) return;
    const measure = () => {
      const rect = anchor.getBoundingClientRect();
      const above = Math.max(0, rect.top - 14);
      const below = Math.max(0, window.innerHeight - rect.bottom - 14);
      const preferred = placement === "below" ? below : above;
      const other = placement === "below" ? above : below;
      const flip = preferred < 144 && other > preferred;
      const side = flip ? placement === "below" ? "above" : "below" : placement;
      const height = Math.floor(Math.min(320, window.innerHeight * 0.4, flip ? other : preferred));
      setSpace((old) => old?.side === side && old.height === height ? old : { side, height });
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(anchor);
    window.addEventListener("resize", measure);
    window.addEventListener("scroll", measure, true);
    return () => {
      observer.disconnect();
      window.removeEventListener("resize", measure);
      window.removeEventListener("scroll", measure, true);
    };
  }, [placement]);
  const activeToken = items[index]?.insert;
  useEffect(() => {
    const viewport = list.current;
    const option = viewport?.querySelector<HTMLElement>(`[data-index="${index}"]`);
    if (!viewport || !option) return;
    const outer = viewport.getBoundingClientRect();
    const inner = option.getBoundingClientRect();
    if (inner.top < outer.top) viewport.scrollTop -= outer.top - inner.top;
    if (inner.bottom > outer.bottom) viewport.scrollTop += inner.bottom - outer.bottom;
  }, [index, activeToken, space?.height]);
  if (!trigger) return null;
  const files = trigger.sigil === "@";
  const title = files ? "Files" : trigger.sigil === "$" || skillsOnly ? "Skills" : "Commands and skills";
  const message = loading ? `Loading ${files ? "files" : skillsOnly || trigger.sigil === "$" ? "skills" : "suggestions"}…`
    : error ? `Could not load ${title.toLowerCase()}: ${error}`
    : emptyReason ?? (trigger.query ? `No matches for “${trigger.query}”.` : files ? "No workspace files found." : `No ${title.toLowerCase()} found.`);
  return (
    <div ref={panel} style={{ maxHeight: space?.height }}
      className={`glass-strong absolute inset-x-0 z-20 flex max-h-[min(20rem,40vh)] flex-col overflow-hidden rounded-2xl ${(space?.side ?? placement) === "below" ? "top-[calc(100%+6px)]" : "bottom-[calc(100%+6px)]"}`}>
      <div className="flex h-8 shrink-0 items-center justify-between border-b border-line/70 px-3 text-xs font-medium text-muted">
        {title}
        {loading && <Loader size={12} />}
      </div>
      <div id={listId} className="overflow-y-auto" ref={list} role="listbox" aria-label={title} aria-busy={loading}>
        {items.map((item, at) => {
          const Icon = item.kind === "skill" || item.insert.startsWith("$") ? IconSparkles : files ? IconFile : IconTerminal2;
          return (
            <button
              id={`${listId}-${at}`}
              key={item.insert}
              type="button"
              role="option"
              tabIndex={-1}
              aria-selected={at === index}
              data-index={at}
              className={`mx-1 grid min-h-8 w-[calc(100%-0.5rem)] cursor-pointer grid-cols-[14px_minmax(0,1fr)_auto] items-center gap-2.5 rounded-lg border-0 px-2 py-1.5 first:mt-1 last:mb-1 text-left text-fg ${at === index ? "bg-accent-soft" : "bg-transparent"}`}
              onMouseDown={(event) => event.preventDefault()}
              onMouseEnter={() => setIndex(at)}
              onClick={() => pick(item)}
            >
              <Icon size={14} className={at === index ? "text-accent" : "text-muted"} />
              <span className="min-w-0">
                <span className="flex min-w-0 items-baseline gap-2">
                  <span className="min-w-0 shrink-0 truncate font-mono text-xs font-medium" title={item.insert.trim()} style={{ maxWidth: item.hint || item.description ? "58%" : "100%" }}>{item.insert.trim()}</span>
                  {(item.hint || item.description) && <span className="truncate text-xs text-muted" title={item.hint ?? item.description ?? undefined}>{item.hint || item.description}</span>}
                </span>
                {item.hint && item.description && <span className="mt-0.5 block truncate text-xs text-muted" title={item.description}>{item.description}</span>}
              </span>
              {item.source === "project" && <span className="text-[10.5px] text-faint">Project</span>}
              {item.source === "cabin" && <span className="text-[10.5px] text-muted">Cabin</span>}
            </button>
          );
        })}
      </div>
      {!items.length && (
        <div className="overflow-y-auto px-3 py-2.5 text-xs text-muted" role="status">
          <div className="break-words">{message}</div>
          <div className="mt-1 text-faint">Esc to keep typing</div>
        </div>
      )}
    </div>
  );
}
