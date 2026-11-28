import type { ActiveTurn } from "../../lib/chatTimeline";
import { elapsedLabel, entryLabel } from "../../lib/chatTimeline";

/** Compositor-only highlight band sweep over active status text (dsh-claude-style). */
export function SweepText({
  text,
  active,
  className = "",
  title,
}: {
  text: string;
  active: boolean;
  className?: string;
  title?: string;
}) {
  if (!active) {
    return <span className={`min-w-0 truncate ${className}`} title={title ?? text}>{text}</span>;
  }
  return (
    <span className={`chat-sweep ${className}`} title={title ?? text}>
      <span className="chat-sweep-base">{text}</span>
      <span className="chat-sweep-band" aria-hidden>
        <span className="chat-sweep-glyphs">{text}</span>
      </span>
    </span>
  );
}

/** Three oscillating accent dots matching Claude's waiting wave loader. */
export function WaitDots() {
  return (
    <span className="chat-wait-dots" aria-hidden>
      <span className="chat-wait-dot" />
      <span className="chat-wait-dot" />
      <span className="chat-wait-dot" />
    </span>
  );
}

/**
 * The live turn's running row inside the transcript flow (dsh-claude-style):
 * three oscillating accent dots, the sweeping current activity wording, and a
 * quiet elapsed clock. Never floats on top of the composer box.
 */
export function LiveLine({ turn, now }: { turn: ActiveTurn; now: number }) {
  const label = turn.waiting
    ? "Waiting for your response"
    : turn.current
      ? entryLabel(turn.current)
      : turn.streaming
        ? "Writing the reply"
        : "Thinking";
  return (
    <div
      className={`inline-flex min-h-7 max-w-full min-w-0 items-center gap-2 py-0.5 text-[12.5px] ${turn.waiting ? "text-warn" : "text-muted"}`}
      role="status"
      aria-live="polite"
    >
      {turn.waiting
        ? <span className="size-[7px] shrink-0 rounded-full bg-warn" aria-hidden />
        : <WaitDots />}
      <SweepText text={label} active={!turn.waiting} className="font-medium" />
      {/* The timer ticks every second: announce the state, not the clock. */}
      {turn.since != null && (
        <span className="shrink-0 text-xs text-faint tabular-nums" aria-hidden>
          {elapsedLabel(Math.max(0, now - turn.since))}
        </span>
      )}
    </div>
  );
}
