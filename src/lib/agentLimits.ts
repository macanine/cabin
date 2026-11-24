// An agent's plan limits in one or two short lines, the way the agent list
// shows them ("5-hour 94% left · Weekly 41% left"). Values come from the
// provider (Claude Code's /usage, Codex app-server), never from local logs.

export interface LimitWindow {
  label: string;
  usedPercent: number;
  resetsAt: number | null;
}

export interface LimitLine {
  text: string;
  /** Under 10% left: shown stronger, like a warning. */
  low: boolean;
  /** Unix ms of the reset, for a tooltip. */
  resetsAt: number | null;
}

/** "5h" / "five hour" / "Session (5h)" → "5-hour"; "7-day" / "weekly" → "Weekly". */
export function shortLimitLabel(label: string): string {
  const text = label.toLowerCase();
  if (/(^|\b)(5\s*-?\s*h|five[ -]hour|5-hour|session)/.test(text)) return "5-hour";
  if (/(7\s*-?\s*d|seven[ -]day|7-day|week)/.test(text)) {
    const model = label.match(/\b(opus|sonnet|fable|haiku)\b/i);
    return model ? `Weekly ${model[1][0].toUpperCase()}${model[1].slice(1).toLowerCase()}` : "Weekly";
  }
  if (/month/.test(text)) return "Monthly";
  return label.trim();
}

export function limitLines(windows: readonly LimitWindow[]): LimitLine[] {
  return windows.map((window) => {
    const left = Math.max(0, Math.min(100, Math.round(100 - window.usedPercent)));
    return {
      text: left === 0 ? `${shortLimitLabel(window.label)} used up` : `${shortLimitLabel(window.label)} ${left}% left`,
      low: left < 10,
      resetsAt: window.resetsAt,
    };
  });
}

// ---------------------------------------------------------------- forecasts
// A plan window is a fixed span ending at its reset. Assuming the share used
// so far was spent evenly since the window opened, the same pace gives where
// the window lands at reset, or when it runs out first. Usage is bursty, so
// this is a pace estimate, worded that way, and withheld early in a window.

const HOUR = 3_600_000;

/** The window's length: reported minutes, else read from its label. */
export function windowSpanMs(window: Pick<LimitWindow, "label"> & { windowMinutes?: number | null }): number | null {
  if (window.windowMinutes && window.windowMinutes > 0) return window.windowMinutes * 60_000;
  const short = shortLimitLabel(window.label);
  if (short === "5-hour") return 5 * HOUR;
  if (short.startsWith("Weekly")) return 7 * 24 * HOUR;
  if (short === "Monthly") return 30 * 24 * HOUR;
  return null;
}

export type ForecastTone = "ok" | "warn" | "bad";

export interface LimitForecast {
  /** Share used now, 0–100. */
  used: number;
  /** Share of the window's time already passed, 0–1, or null when unknown. */
  elapsed: number | null;
  /** Share used by the reset at the current pace, 0–100 (capped), or null. */
  projected: number | null;
  /** Ms until the reset, or null when the reset time is unknown. */
  resetsIn: number | null;
  /** Ms until the window runs out at this pace, when that comes before the reset. */
  runsOutIn: number | null;
  tone: ForecastTone;
  /** One short sentence: "~46% used by reset", "Runs out in ~40m". */
  text: string;
}

/** "45m", "3h 20m", "2d 4h". */
export function formatSpan(ms: number): string {
  const minutes = Math.max(1, Math.round(ms / 60_000));
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  if (hours < 48) return minutes % 60 ? `${hours}h ${minutes % 60}m` : `${hours}h`;
  const days = Math.floor(hours / 24);
  return hours % 24 ? `${days}d ${hours % 24}h` : `${days}d`;
}

export function forecastLimit(
  window: LimitWindow & { windowMinutes?: number | null },
  now: number,
): LimitForecast {
  const used = Math.max(0, Math.min(100, window.usedPercent));
  const span = windowSpanMs(window);
  const resetsIn = window.resetsAt == null ? null : Math.max(0, window.resetsAt - now);
  const base = { used, resetsIn, runsOutIn: null, projected: null };
  if (used >= 100) {
    return { ...base, elapsed: null, projected: 100, tone: "bad", text: resetsIn != null ? `Used up · resets in ${formatSpan(resetsIn)}` : "Used up" };
  }
  if (span == null || resetsIn == null || resetsIn > span) {
    return { ...base, elapsed: null, tone: used >= 90 ? "bad" : used >= 70 ? "warn" : "ok", text: resetsIn != null ? `Resets in ${formatSpan(resetsIn)}` : "Reset time unknown" };
  }
  const spent = span - resetsIn;
  const elapsed = spent / span;
  // Too little of the window has passed (or nothing used) to read a pace.
  if (elapsed < 0.08 || spent < 10 * 60_000 || used === 0) {
    return { ...base, elapsed, tone: used >= 90 ? "bad" : "ok", text: used === 0 ? "Unused so far" : "Too early to estimate" };
  }
  const rate = used / spent;
  const landing = used + rate * resetsIn;
  if (landing <= 100) {
    const projected = Math.round(landing);
    return { ...base, elapsed, projected, tone: projected >= 85 ? "warn" : "ok", text: `~${projected}% used by reset` };
  }
  const runsOutIn = (100 - used) / rate;
  return {
    ...base, elapsed, projected: 100, runsOutIn,
    tone: "bad",
    text: `Runs out in ~${formatSpan(runsOutIn)}, ${formatSpan(resetsIn - runsOutIn)} before reset`,
  };
}
