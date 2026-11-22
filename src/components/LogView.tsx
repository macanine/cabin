import { IconArrowDown } from "@tabler/icons-react";
import { type ReactNode, useLayoutEffect, useRef, useState } from "react";

/** Distance from the bottom (px) that still counts as "following". */
const FOLLOW_SLACK = 24;

const reducedMotion = () =>
  typeof window !== "undefined" && window.matchMedia?.("(prefers-reduced-motion: reduce)").matches;

/**
 * A bounded, self-scrolling log. New lines scroll it to the bottom smoothly
 * while the reader is at the bottom; scrolling up pauses that until they
 * return (or press "Latest"), so reading older lines is never interrupted.
 */
export function LogView({
  lines,
  label = "Log output",
  empty,
  variant = "plain",
}: {
  lines: string[];
  label?: string;
  /** Shown while there are no lines yet. */
  empty?: ReactNode;
  /** `terminal`: dark console look, for raw process output. */
  variant?: "plain" | "terminal";
}) {
  const ref = useRef<HTMLPreElement>(null);
  const follow = useRef(true);
  const lastTop = useRef(0);
  const first = useRef(true);
  const [paused, setPaused] = useState(false);
  const text = lines.join("\n");

  const toBottom = (smooth: boolean) => {
    const el = ref.current;
    if (!el) return;
    el.scrollTo({ top: el.scrollHeight, behavior: smooth && !reducedMotion() ? "smooth" : "auto" });
  };

  useLayoutEffect(() => {
    if (!follow.current) return;
    // Jump on first paint; glide for lines that arrive afterwards.
    toBottom(!first.current);
    first.current = false;
  }, [text]);

  const onScroll = () => {
    const el = ref.current;
    if (!el) return;
    const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight <= FOLLOW_SLACK;
    // Only an upward scroll is the reader's: smooth auto-scroll and new
    // content never move scrollTop up.
    if (el.scrollTop < lastTop.current - 1 && !atBottom) follow.current = false;
    else if (atBottom) follow.current = true;
    lastTop.current = el.scrollTop;
    setPaused(!follow.current);
  };

  return (
    <div className={`log-view ${variant}`}>
      <pre ref={ref} className="log-view-body" onScroll={onScroll} tabIndex={0} aria-label={label}>
        {lines.length > 0 ? text : <span className="faint">{empty}</span>}
      </pre>
      {paused && (
        <button
          className="log-view-latest"
          onClick={() => {
            follow.current = true;
            setPaused(false);
            toBottom(true);
          }}
        >
          <IconArrowDown size={12} stroke={2.2} />
          Latest
        </button>
      )}
    </div>
  );
}
