// Unified plan-limit labels, pacing calculations, and Magpie-style reset
// timeline helpers shared across Agents, Usage, and account panels.
// Values come from each provider (Claude Code's /usage, Codex app-server),
// never fabricated from local token logs.

export interface LimitWindow {
  label: string;
  usedPercent: number;
  resetsAt: number | null;
  windowMinutes?: number | null;
  resetText?: string | null;
}

export type WindowTier = "5h" | "7d" | "other";

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

/** Classify a window into the unified 5h, 7d, or longer/other bucket. */
export function classifyWindowTier(window: Pick<LimitWindow, "label" | "windowMinutes">): WindowTier {
  if (window.windowMinutes && window.windowMinutes > 0) {
    if (window.windowMinutes <= 360) return "5h";
    if (window.windowMinutes <= 14 * 24 * 60) return "7d";
    return "other";
  }
  const short = shortLimitLabel(window.label);
  if (short === "5-hour") return "5h";
  if (short.startsWith("Weekly")) return "7d";
  return "other";
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
// A plan window is a fixed span ending at its reset. Comparing used share
// against elapsed time share yields both the projected landing at reset and
// the relative pace ratio vs. even consumption (1.0x = exact diagonal pace).

export const HOUR_MS = 3_600_000;
export const FIVE_HOUR_MS = 5 * HOUR_MS;
export const SEVEN_DAY_MS = 7 * 24 * HOUR_MS;

/** The window's length: reported minutes, else read from its label. */
export function windowSpanMs(window: Pick<LimitWindow, "label" | "windowMinutes">): number | null {
  if (window.windowMinutes && window.windowMinutes > 0) return window.windowMinutes * 60_000;
  const short = shortLimitLabel(window.label);
  if (short === "5-hour") return FIVE_HOUR_MS;
  if (short.startsWith("Weekly")) return SEVEN_DAY_MS;
  if (short === "Monthly") return 30 * 24 * HOUR_MS;
  return null;
}

export type ForecastTone = "ok" | "warn" | "bad";
export type PaceStatus = "under" | "even" | "fast" | "exhausted" | "early" | "unknown";

export interface LimitForecast {
  /** Share used now, 0–100. */
  used: number;
  /** Share remaining now, 0–100. */
  left: number;
  /** Share of the window's time already passed, 0–1, or null when unknown. */
  elapsed: number | null;
  /** Share used by the reset at the current pace, 0–100 (capped), or null. */
  projected: number | null;
  /** Relative burn speed vs even diagonal pace (`used / (elapsed * 100)`), or null. */
  paceRatio: number | null;
  /** Difference from even pace in percentage points (`used - elapsed * 100`). */
  deltaFromEven: number | null;
  /** Sustainable remaining burn budget (% per hour for <=24h windows, else % per day). */
  sustainableRate: { value: number; unit: "h" | "d" } | null;
  /** Semantic pace bucket for badges and styling. */
  status: PaceStatus;
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

function sustainableBudget(left: number, resetsIn: number, span: number): { value: number; unit: "h" | "d" } | null {
  if (left <= 0 || resetsIn <= 60_000) return null;
  if (span <= 24 * HOUR_MS) {
    return { value: Math.min(100, left / (resetsIn / HOUR_MS)), unit: "h" };
  }
  return { value: Math.min(100, left / (resetsIn / (24 * HOUR_MS))), unit: "d" };
}

export function forecastLimit(
  window: LimitWindow,
  now: number,
): LimitForecast {
  const used = Math.max(0, Math.min(100, window.usedPercent));
  const left = Math.max(0, 100 - used);
  const span = windowSpanMs(window);
  const resetsIn = window.resetsAt == null ? null : Math.max(0, window.resetsAt - now);
  const base = {
    used,
    left,
    resetsIn,
    runsOutIn: null as number | null,
    projected: null as number | null,
    paceRatio: null as number | null,
    deltaFromEven: null as number | null,
    sustainableRate: span != null && resetsIn != null ? sustainableBudget(left, resetsIn, span) : null,
  };

  if (used >= 100) {
    return {
      ...base,
      elapsed: null,
      projected: 100,
      status: "exhausted",
      tone: "bad",
      text: resetsIn != null ? `Used up · resets in ${formatSpan(resetsIn)}` : "Used up",
    };
  }
  if (span == null || resetsIn == null || resetsIn > span) {
    return {
      ...base,
      elapsed: null,
      status: "unknown",
      tone: used >= 90 ? "bad" : used >= 70 ? "warn" : "ok",
      text: resetsIn != null ? `Resets in ${formatSpan(resetsIn)}` : "Reset time unknown",
    };
  }

  const spent = span - resetsIn;
  const elapsed = spent / span;
  const deltaFromEven = Math.round(used - elapsed * 100);

  // Too little of the window has passed (or nothing used) to read a pace.
  if (elapsed < 0.08 || spent < 10 * 60_000 || used === 0) {
    return {
      ...base,
      elapsed,
      deltaFromEven,
      status: "early",
      tone: used >= 90 ? "bad" : "ok",
      text: used === 0 ? "Unused so far" : "Too early to estimate",
    };
  }

  const rate = used / spent;
  const paceRatio = Number((used / (elapsed * 100)).toFixed(2));
  const landing = used + rate * resetsIn;

  if (landing <= 100) {
    const projected = Math.round(landing);
    const status: PaceStatus = paceRatio >= 0.92 ? "even" : "under";
    return {
      ...base,
      elapsed,
      projected,
      paceRatio,
      deltaFromEven,
      status,
      tone: projected >= 85 ? "warn" : "ok",
      text: `~${projected}% used by reset`,
    };
  }

  const runsOutIn = (100 - used) / rate;
  return {
    ...base,
    elapsed,
    projected: 100,
    paceRatio,
    deltaFromEven,
    status: "fast",
    runsOutIn,
    tone: "bad",
    text: `Runs out in ~${formatSpan(runsOutIn)}, ${formatSpan(resetsIn - runsOutIn)} before reset`,
  };
}

// --------------------------------------------------- Magpie reset timeline
// Groups windows resetting within a horizon (Next 5 hours / Next 7 days) along
// a 0..1 time axis ("now" at 0, horizon at 1). Windows resetting within the
// cluster tolerance sit together so concurrent resets never overlap.

export interface TimelineEntry<T> {
  item: T;
  resetsAt: number;
  /** 0 = now, 1 = end of horizon. */
  position: number;
}

export interface TimelineCluster<T> {
  /** Representative normalized position (0..1) for the cluster along the axis. */
  position: number;
  /** Earliest reset time in this cluster. */
  resetsAt: number;
  entries: TimelineEntry<T>[];
}

export function buildResetTimeline<T extends { resetsAt: number | null }>(
  items: readonly T[],
  horizonMs: number,
  now: number,
  options: { minRemainingMs?: number; clusterWindowMs?: number } = {},
): TimelineCluster<T>[] {
  const minMs = options.minRemainingMs ?? 0;
  const clusterMs = options.clusterWindowMs ?? (horizonMs <= FIVE_HOUR_MS ? 10 * 60_000 : 3 * HOUR_MS);

  const entries: TimelineEntry<T>[] = items
    .filter((item): item is T & { resetsAt: number } => {
      if (item.resetsAt == null) return false;
      const remain = item.resetsAt - now;
      return remain > minMs && remain <= horizonMs;
    })
    .sort((a, b) => a.resetsAt - b.resetsAt)
    .map((item) => ({
      item,
      resetsAt: item.resetsAt,
      position: Math.max(0.02, Math.min(0.98, (item.resetsAt - now) / horizonMs)),
    }));

  const clusters: TimelineCluster<T>[] = [];
  for (const entry of entries) {
    const last = clusters[clusters.length - 1];
    if (last && entry.resetsAt - last.resetsAt <= clusterMs) {
      last.entries.push(entry);
    } else {
      clusters.push({ position: entry.position, resetsAt: entry.resetsAt, entries: [entry] });
    }
  }
  return clusters;
}
