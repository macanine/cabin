import { useLayoutEffect, useRef, useState, type DependencyList } from "react";

/** Distance from the bottom (px) within which the view keeps following. */
const FOLLOW_THRESHOLD = 80;

/**
 * Keep a transcript pinned to its end while the reader is there. Scrolling
 * up stops following (and counts what arrives meanwhile); returning to the
 * end, or `jump()`, resumes it. Opening a fold never scrolls: only `deps`
 * changes do.
 */
export function useFollowScroll(deps: DependencyList, newCount: number) {
  const ref = useRef<HTMLDivElement>(null);
  const follow = useRef(true);
  const [atEnd, setAtEnd] = useState(true);
  const [unseenFrom, setUnseenFrom] = useState<number | null>(null);

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
    resume() { follow.current = true; },
    jump() {
      const el = ref.current;
      if (!el) return;
      follow.current = true;
      setUnseenFrom(null);
      el.scrollTo({ top: el.scrollHeight, behavior: "smooth" });
    },
  };
}
