import { useEffect, useId, useMemo } from "react";
import { windowSpanMs, type LimitForecast, type LimitWindow } from "../lib/agentLimits";
import { burnGeometry, historyKey, recordSample, samplesFor } from "../lib/limitHistory";
import { formatDate, formatTime } from "../lib/time";

const W = 100;

/** Whether a window can be drawn: its length and reset are known and it is under way. */
export function burnable(window: LimitWindow & { windowMinutes?: number | null }, now: number): boolean {
  const span = windowSpanMs(window);
  return span != null && window.resetsAt != null && window.resetsAt > now && window.resetsAt - now <= span;
}

function path(points: [number, number][], height: number): string {
  return points.map(([x, y], i) => `${i ? "L" : "M"}${(x * W).toFixed(2)} ${((1 - y) * height).toFixed(2)}`).join(" ");
}

/**
 * A plan window as a burn line: the window opens at 0%, the solid line runs
 * through every reading Cabin has seen, and a dotted line carries the pace
 * on to the reset, or to the moment it runs out. Without a reset time or a
 * known window length the chart has nothing honest to draw, so it is omitted.
 */
export function BurnChart({ window, forecast, scope, now, height = 56, axis = false }: {
  window: LimitWindow & { windowMinutes?: number | null };
  forecast: LimitForecast;
  /** Whose window this is (agent and account), so lines never mix accounts. */
  scope: string;
  now: number;
  height?: number;
  /** Show the window's opening and reset times under the chart. */
  axis?: boolean;
}) {
  const gradient = `burn-${useId().replace(/[^\w-]/g, "")}`;
  const span = windowSpanMs(window);
  const resetsAt = window.resetsAt;
  const used = Math.max(0, Math.min(100, window.usedPercent));
  const key = resetsAt != null ? historyKey(scope, window.label, resetsAt) : null;

  // Keep each new reading; the effect runs when the provider's value changes.
  useEffect(() => {
    if (key && resetsAt != null) recordSample(key, resetsAt, { t: Date.now(), used });
  }, [key, resetsAt, used]);

  const geometry = useMemo(() => {
    if (!key || resetsAt == null || span == null || resetsAt - now > span) return null;
    return burnGeometry(samplesFor(key), { t: Math.min(now, resetsAt), used }, resetsAt, span);
  }, [key, resetsAt, span, now, used]);
  if (!geometry || resetsAt == null || span == null) return null;

  const { actual, projection, exhaust, now: nowX } = geometry;
  const last = actual[actual.length - 1];
  const area = `${path(actual, height)} L${(last[0] * W).toFixed(2)} ${height} L0 ${height} Z`;
  // Short windows read in clock times, long ones in dates.
  const stamp = (ms: number) => span > 86_400_000 ? formatDate(ms, { month: "short", day: "numeric" }) : formatTime(ms);
  const at = (x: number, y: number) => ({ left: `${x * 100}%`, top: `${(1 - y) * 100}%` });

  return (
    <div className="burn" data-tone={forecast.tone}>
      <div className="burn-plot" style={{ height }}>
        <svg viewBox={`0 0 ${W} ${height}`} preserveAspectRatio="none" aria-hidden>
          <defs>
            <linearGradient id={gradient} x1="0" y1="0" x2="0" y2="1">
              <stop offset="0%" stopColor="var(--burn-color)" stopOpacity="0.28" />
              <stop offset="100%" stopColor="var(--burn-color)" stopOpacity="0" />
            </linearGradient>
          </defs>
          <line className="burn-ceiling" x1="0" x2={W} y1="0.5" y2="0.5" />
          <line className="burn-now" x1={nowX * W} x2={nowX * W} y1="0" y2={height} />
          <path d={area} fill={`url(#${gradient})`} />
          {projection.length > 1 && <path className="burn-projection" d={path(projection, height)} />}
          <path className="burn-line" d={path(actual, height)} />
        </svg>
        {exhaust && <span className="burn-exhaust" style={at(...exhaust)} />}
        <span className="burn-dot" style={at(...last)} />
      </div>
      {axis && (
        <div className="burn-axis">
          <span>{stamp(resetsAt - span)}</span>
          {nowX > 0.14 && nowX < 0.86 && <span className="burn-axis-now" style={{ left: `${nowX * 100}%` }}>now</span>}
          <span>{stamp(resetsAt)}</span>
        </div>
      )}
    </div>
  );
}
