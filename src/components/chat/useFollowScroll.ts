import { useEffect, useLayoutEffect, useRef, useState, type DependencyList } from "react";

/** Distance from the bottom (px) within which the view keeps following. */
const FOLLOW_THRESHOLD = 80;

/**
 * Keep a transcript pinned to its end while the reader is there (dsh-claude-style D95):
 * - User scroll intent is sacred: an upward wheel or touch gesture yields follow
 *   immediately so streaming updates never yank the reader back down.
 * - Height growth inside the transcript (streaming blocks, Shiki hydration, images)
 *   coalesces into a single animation frame while following.
 * - Opening a fold while scrolled up never scrolls.
 */
export function useFollowScroll(deps: DependencyList, newCount: number) {
  const ref = useRef<HTMLDivElement>(null);
  const follow = useRef(true);
  const rafId = useRef<number | null>(null);
  const [atEnd, setAtEnd] = useState(true);
  const [unseenFrom, setUnseenFrom] = useState<number | null>(null);

  function schedulePin() {
    if (!follow.current || rafId.current !== null) return;
    rafId.current = requestAnimationFrame(() => {
      rafId.current = null;
      const el = ref.current;
      if (!follow.current || !el) return;
      el.scrollTop = el.scrollHeight;
    });
  }

  // Yield follow immediately when the reader scrolls up with wheel/trackpad or touch.
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const onWheel = (event: WheelEvent) => {
      if (event.deltaY < 0) {
        follow.current = false;
      }
    };
    let touchStartY: number | null = null;
    const onTouchStart = (event: TouchEvent) => {
      touchStartY = event.touches[0]?.clientY ?? null;
    };
    const onTouchMove = (event: TouchEvent) => {
      const y = event.touches[0]?.clientY;
      if (touchStartY != null && y != null && y > touchStartY + 4) {
        follow.current = false;
      }
    };

    el.addEventListener("wheel", onWheel, { passive: true });
    el.addEventListener("touchstart", onTouchStart, { passive: true });
    el.addEventListener("touchmove", onTouchMove, { passive: true });

    // Follow smooth content height growth (streaming tokens, code blocks, tables).
    const content = el.firstElementChild;
    const observer = content && typeof ResizeObserver !== "undefined"
      ? new ResizeObserver(() => {
          if (follow.current) schedulePin();
        })
      : null;
    if (observer && content) observer.observe(content);

    return () => {
      el.removeEventListener("wheel", onWheel);
      el.removeEventListener("touchstart", onTouchStart);
      el.removeEventListener("touchmove", onTouchMove);
      observer?.disconnect();
      if (rafId.current !== null) {
        cancelAnimationFrame(rafId.current);
        rafId.current = null;
      }
    };
  }, []);

  useLayoutEffect(() => {
    const el = ref.current;
    if (follow.current && el) el.scrollTop = el.scrollHeight;
  }, deps); // eslint-disable-line react-hooks/exhaustive-deps

  function onScroll() {
    const el = ref.current;
    if (!el) return;
    const end = el.scrollHeight - el.scrollTop - el.clientHeight < FOLLOW_THRESHOLD;
    follow.current = end;
    setAtEnd(end);
    if (end) setUnseenFrom(null);
    else setUnseenFrom((from) => from ?? newCount);
  }

  return {
    ref,
    atEnd,
    /** Rows that arrived since the reader scrolled away. */
    unseen: unseenFrom == null ? 0 : Math.max(0, newCount - unseenFrom),
    onScroll,
    /** Follow again from the next update (after sending). */
    resume() {
      follow.current = true;
      setAtEnd(true);
      setUnseenFrom(null);
      schedulePin();
    },
    jump() {
      const el = ref.current;
      if (!el) return;
      follow.current = true;
      setAtEnd(true);
      setUnseenFrom(null);
      el.scrollTo({ top: el.scrollHeight, behavior: "smooth" });
    },
  };
}
