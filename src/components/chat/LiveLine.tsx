import type { ActiveTurn } from "../../lib/chatTimeline";
import { elapsedLabel, entryLabel } from "../../lib/chatTimeline";

/** What the agent is doing right now, and for how long. */
export function LiveLine({ turn, now }: { turn: ActiveTurn; now: number }) {
  const label = turn.waiting ? "Waiting for you" : turn.current ? entryLabel(turn.current) : turn.streaming ? "Writing the reply" : "Working";
  return (
    <span className={`inline-flex h-7 min-w-0 items-center gap-2 px-1.5 text-xs ${turn.waiting ? "text-warn" : "text-muted"}`} role="status">
      <span className={`size-[7px] shrink-0 rounded-full ${turn.waiting ? "bg-warn" : "live-dot bg-ok"}`} aria-hidden />
      <span className="min-w-0 truncate font-medium" title={label}>{label}</span>
      {/* The timer ticks every second: announce the state, not the clock. */}
      {turn.since != null && <span className="shrink-0 text-faint tabular-nums" aria-hidden>{elapsedLabel(Math.max(0, now - turn.since))}</span>}
    </span>
  );
}
