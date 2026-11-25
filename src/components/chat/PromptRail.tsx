import { useEffect, useState, type RefObject } from "react";
import type { TimelineRow } from "../../lib/chatTimeline";

/** Prompts shown at once; a longer conversation keeps the newest. */
const LIMIT = 24;
/** Below this many there is nothing to navigate. */
const MIN = 3;

/**
 * The conversation's prompts as a column of ticks beside the transcript: the
 * one being read is lit, hovering names them, a click jumps there.
 */
export function PromptRail({ rows, scrollRef }: { rows: TimelineRow[]; scrollRef: RefObject<HTMLDivElement | null> }) {
  const prompts = rows.filter((row) => row.kind === "user").slice(-LIMIT);
  const [current, setCurrent] = useState<string | null>(null);
  const ids = prompts.map((row) => row.id).join("\n");

  useEffect(() => {
    const scroller = scrollRef.current;
    if (!scroller || !ids) return;
    let frame = 0;
    const measure = () => {
      frame = 0;
      const line = scroller.getBoundingClientRect().top + scroller.clientHeight * 0.35;
      let at: string | null = null;
      for (const anchor of scroller.querySelectorAll<HTMLElement>("[data-turn-anchor]")) {
        if (at !== null && anchor.getBoundingClientRect().top > line) break;
        at = anchor.dataset.turnAnchor ?? null;
      }
      setCurrent(at);
    };
    const schedule = () => { frame ||= requestAnimationFrame(measure); };
    measure();
    scroller.addEventListener("scroll", schedule, { passive: true });
    return () => {
      cancelAnimationFrame(frame);
      scroller.removeEventListener("scroll", schedule);
    };
  }, [scrollRef, ids]);

  if (prompts.length < MIN) return null;
  const jump = (id: string) => {
    const calm = window.matchMedia("(prefers-reduced-motion: reduce)").matches || document.documentElement.dataset.perf === "lite";
    scrollRef.current?.querySelector(`[data-turn-anchor="${CSS.escape(id)}"]`)?.scrollIntoView({ block: "start", behavior: calm ? "auto" : "smooth" });
  };
  return (
    <nav className="prompt-rail" aria-label="Prompts in this conversation">
      {prompts.map((row) => {
        const text = row.kind === "user" ? row.item.text.trim().split("\n")[0] || "Attachment" : "";
        return (
          <button key={row.id} type="button" aria-label={`Go to: ${text}`} aria-current={row.id === current ? "location" : undefined} onClick={() => jump(row.id)}>
            <span className="prompt-rail-tick" aria-hidden />
            <span className="prompt-rail-text" aria-hidden>{text}</span>
          </button>
        );
      })}
    </nav>
  );
}
