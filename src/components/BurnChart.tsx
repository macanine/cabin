import { useEffect, useId, useMemo, useState, type PointerEvent as ReactPointerEvent } from "react";
import { windowSpanMs, type LimitForecast, type LimitWindow } from "../lib/agentLimits";
import { burnGeometry, historyKey, recordSample, samplesFor } from "../lib/limitHistory";
import { formatDate, formatTime } from "../lib/time";
import { useI18n } from "../i18n";

const W = 100;

/** Whether a window can be drawn: its length and reset are known and it is under way. */
export function burnable(window: LimitWindow, now: number): boolean {
  const span = windowSpanMs(window);
  return span != null && window.resetsAt != null && window.resetsAt > now && window.resetsAt - now <= span;
}

function path(points: [number, number][], height: number): string {
  return points.map(([x, y], i) => `${i ? "L" : "M"}${(x * W).toFixed(2)} ${((1 - y) * height).toFixed(2)}`).join(" ");
}

function interpolateY(points: [number, number][], targetX: number): number | null {
  if (points.length === 0) return null;
  if (targetX <= points[0][0]) return points[0][1];
  for (let i = 1; i < points.length; i++) {
    const [x0, y0] = points[i - 1];
    const [x1, y1] = points[i];
    if (targetX <= x1) {
      const span = x1 - x0;
      const ratio = span > 0 ? (targetX - x0) / span : 1;
      return y0 + (y1 - y0) * ratio;
    }
  }
  return points[points.length - 1][1];
}

/**
 * Magpie-inspired pacing curve chart:
 * - X axis: window open -> now -> reset
 * - Y axis: 0% -> 100% quota used
 * - 45° diagonal dashed guideline shows exact even pace (1.0×);
 *   below diagonal = calm headroom, above diagonal = burning ahead of pace
 * - Solid area/line follows observed readings to a live dot at `now`
 * - Dotted ray projects current pace to reset or 100% exhaustion
 * - Interactive pointer hover reveals time, even-pace target, and actual/projected %
 */
export function BurnChart({
  window,
  forecast,
  scope,
  now,
  height = 82,
  axis = false,
  interactive = true,
}: {
  window: LimitWindow;
  forecast: LimitForecast;
  /** Whose window this is (agent and account), so lines never mix accounts. */
  scope: string;
  now: number;
  height?: number;
  /** Show the window's opening, now, and reset times under the chart. */
  axis?: boolean;
  /** Enable pointer hover scrub readout. */
  interactive?: boolean;
}) {
  const { t } = useI18n();
  const gradient = `burn-${useId().replace(/[^\w-]/g, "")}`;
  const span = windowSpanMs(window);
  const resetsAt = window.resetsAt;
  const used = Math.max(0, Math.min(100, window.usedPercent));
  const key = resetsAt != null ? historyKey(scope, window.label, resetsAt) : null;
  const [hoverX, setHoverX] = useState<number | null>(null);

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
  const start = resetsAt - span;
  const last = actual[actual.length - 1];
  const area = `${path(actual, height)} L${(last[0] * W).toFixed(2)} ${height} L0 ${height} Z`;
  const stamp = (ms: number) =>
    span > 86_400_000
      ? formatDate(ms, { month: "short", day: "numeric" })
      : formatTime(ms);
  const fullStamp = (ms: number) =>
    span > 86_400_000
      ? `${formatDate(ms, { month: "short", day: "numeric" })} ${formatTime(ms)}`
      : formatTime(ms);
  const at = (x: number, y: number) => ({ left: `${x * 100}%`, top: `${(1 - y) * 100}%` });

  const onPointerMove = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (!interactive) return;
    const rect = event.currentTarget.getBoundingClientRect();
    if (rect.width <= 0) return;
    const x = Math.max(0, Math.min(1, (event.clientX - rect.left) / rect.width));
    setHoverX(x);
  };

  let hoverInfo: { x: number; y: number; time: string; pct: number; evenPct: number; projected: boolean } | null = null;
  if (hoverX != null) {
    const isFuture = hoverX > nowX && projection.length > 1;
    const yVal = isFuture ? interpolateY(projection, hoverX) : interpolateY(actual, Math.min(hoverX, nowX));
    if (yVal != null) {
      hoverInfo = {
        x: hoverX,
        y: yVal,
        time: fullStamp(start + hoverX * span),
        pct: Math.round(yVal * 100),
        evenPct: Math.round(hoverX * 100),
        projected: isFuture,
      };
    }
  }

  return (
    <div className="burn" data-tone={forecast.tone}>
      <div
        className={`burn-plot${interactive ? " interactive" : ""}`}
        style={{ height }}
        onPointerMove={onPointerMove}
        onPointerLeave={() => setHoverX(null)}
      >
        <svg viewBox={`0 0 ${W} ${height}`} preserveAspectRatio="none" aria-hidden>
          <defs>
            <linearGradient id={gradient} x1="0" y1="0" x2="0" y2="1">
              <stop offset="0%" stopColor="var(--burn-color)" stopOpacity="0.26" />
              <stop offset="100%" stopColor="var(--burn-color)" stopOpacity="0.02" />
            </linearGradient>
          </defs>
          {/* Horizontal reference gridlines at 25%, 50%, 75%, 100% */}
          <line className="burn-grid" x1="0" x2={W} y1={height * 0.25} y2={height * 0.25} />
          <line className="burn-grid" x1="0" x2={W} y1={height * 0.5} y2={height * 0.5} />
          <line className="burn-grid" x1="0" x2={W} y1={height * 0.75} y2={height * 0.75} />
          <line className="burn-ceiling" x1="0" x2={W} y1="0.5" y2="0.5" />
          {/* 45° Magpie even-pace reference diagonal: 0% at start → 100% at reset */}
          <line className="burn-even" x1="0" y1={height} x2={W} y2="0" />
          {/* Current time hairline */}
          <line className="burn-now" x1={nowX * W} x2={nowX * W} y1="0" y2={height} />
          {/* Observed area & curve */}
          <path d={area} fill={`url(#${gradient})`} />
          {/* Projected pace continuation */}
          {projection.length > 1 && <path className="burn-projection" d={path(projection, height)} />}
          <path className="burn-line" d={path(actual, height)} />
          {/* Hover crosshair */}
          {hoverInfo && <line className="burn-hover-line" x1={hoverInfo.x * W} x2={hoverInfo.x * W} y1="0" y2={height} />}
        </svg>

        <span className="burn-even-tag" aria-hidden>{t("quota.evenPaceShort")}</span>
        {exhaust && <span className="burn-exhaust" style={at(...exhaust)} />}
        <span className="burn-dot" style={at(...last)} />

        {hoverInfo && (
          <>
            <span className="burn-hover-dot" style={at(hoverInfo.x, hoverInfo.y)} />
            <div
              className="burn-tooltip"
              style={{
                left: `${Math.max(18, Math.min(82, hoverInfo.x * 100))}%`,
              }}
            >
              <strong>{hoverInfo.projected ? `~${hoverInfo.pct}%` : `${hoverInfo.pct}%`}</strong>
              <span>{t("quota.evenRef", { pct: hoverInfo.evenPct })}</span>
              <span className="burn-tooltip-time">{hoverInfo.time}</span>
            </div>
          </>
        )}
      </div>

      {axis && (
        <div className="burn-axis">
          <span>{stamp(start)}</span>
          {nowX > 0.2 && nowX < 0.8 && (
            <span className="burn-axis-now" style={{ left: `${nowX * 100}%` }}>
              {t("quota.now")}
            </span>
          )}
          <span>{stamp(resetsAt)}</span>
        </div>
      )}
    </div>
  );
}
