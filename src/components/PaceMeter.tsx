import type { LimitForecast } from "../lib/agentLimits";

/**
 * A plan window as one bar: solid for what is used, a translucent stretch to
 * where the current pace lands at reset, and a tick for how much of the
 * window's time has passed (fill beyond the tick = faster than even pace).
 */
export function PaceMeter({ forecast, size = "md", label }: { forecast: LimitForecast; size?: "sm" | "md"; label: string }) {
  const { used, projected, elapsed, tone } = forecast;
  return (
    <span className="pace" data-size={size} data-tone={tone} role="meter" aria-label={label}
      aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(used)} aria-valuetext={`${Math.round(used)}% used. ${forecast.text}`}>
      {projected != null && projected > used && <span className="pace-ghost" style={{ inlineSize: `${projected}%` }} />}
      <span className="pace-fill" style={{ inlineSize: `${Math.max(used > 0 ? 1.5 : 0, used)}%` }} />
      {elapsed != null && <span className="pace-tick" style={{ insetInlineStart: `${Math.min(99.5, elapsed * 100)}%` }} />}
    </span>
  );
}
