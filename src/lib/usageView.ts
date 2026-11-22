import type { UsageCounts, UsageDaily, UsageModel } from "../services/ipc/generated/core";
import { formatCount, formatUsd } from "./format.ts";

export type UsageRange = "1" | "7" | "30" | "90" | "all";
export type UsageMetric = "cost" | "tokens" | "requests";

export const USAGE_RANGES: { value: UsageRange; label: string }[] = [
  { value: "1", label: "Today" },
  { value: "7", label: "7D" },
  { value: "30", label: "30D" },
  { value: "90", label: "90D" },
  { value: "all", label: "All" },
];

type Tokens = Pick<UsageCounts, "input" | "output" | "cacheRead" | "cacheWrite">;

export const totalTokens = (value: Tokens) => value.input + value.output + value.cacheRead + value.cacheWrite;

/** Cache read / (input + cache read + cache write); null without input. */
export function cacheHitRate(value: Tokens): number | null {
  const input = value.input + value.cacheRead + value.cacheWrite;
  return input ? value.cacheRead / input : null;
}

export function metricOf(value: Tokens & Pick<UsageCounts, "cost" | "requests">, metric: UsageMetric): number {
  return metric === "cost" ? value.cost : metric === "tokens" ? totalTokens(value) : value.requests;
}

export function formatMetric(value: number, metric: UsageMetric): string {
  return metric === "cost" ? formatUsd(value) : formatCount(value);
}

/** Start of the range in Mac-local days (inclusive), or null for all time. */
export function rangeStart(range: UsageRange, now: number = Date.now()): number | null {
  if (range === "all") return null;
  const start = new Date(now);
  start.setHours(0, 0, 0, 0);
  start.setDate(start.getDate() - (Number(range) - 1));
  return start.getTime();
}

export interface DayBar<T> {
  /** `YYYY-MM-DD`, Mac-local. */
  day: string;
  value: number;
  item: T | null;
}

const pad = (n: number) => String(n).padStart(2, "0");
const dayKey = (date: Date) => `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`;

/**
 * One bar per day from `first` to `last` (inclusive, `YYYY-MM-DD`), with
 * days without activity at zero, so gaps read as quiet days, not as missing
 * data. At most `limit` bars, keeping the most recent.
 */
export function dayBars<T extends { day: string }>(
  items: T[],
  value: (item: T) => number,
  first: string,
  last: string,
  limit = 180,
): DayBar<T>[] {
  const byDay = new Map(items.map((item) => [item.day, item]));
  const [fy, fm, fd] = first.split("-").map(Number);
  const [ly, lm, ld] = last.split("-").map(Number);
  const end = new Date(ly, lm - 1, ld);
  const cursor = new Date(fy, fm - 1, fd);
  const bars: DayBar<T>[] = [];
  // Walk by calendar day (DST-safe: setDate, not +24h).
  while (cursor <= end && bars.length < 4000) {
    const key = dayKey(cursor);
    const item = byDay.get(key) ?? null;
    bars.push({ day: key, value: item ? value(item) : 0, item });
    cursor.setDate(cursor.getDate() + 1);
  }
  return bars.slice(-limit);
}

/** "2026-11-20" → "Nov 20" (English, like the rest of the app's numbers). */
export function shortDay(day: string): string {
  const [y, m, d] = day.split("-").map(Number);
  return new Date(y, m - 1, d).toLocaleDateString("en-US", { month: "short", day: "numeric" });
}

export interface ShareRow {
  key: string;
  value: number;
  /** Fraction of the largest row, for the bar. */
  share: number;
  /** Fraction of the total. */
  percent: number;
  requests: number;
}

/** Models ranked by the metric, the rest folded into "Other" past `top`. */
export function modelShares(models: UsageModel[], metric: UsageMetric, top = 6): ShareRow[] {
  const ranked = models
    .map((model) => ({ key: model.model, value: metricOf(model, metric), requests: model.requests }))
    .filter((row) => row.value > 0)
    .sort((a, b) => b.value - a.value || a.key.localeCompare(b.key));
  const head = ranked.slice(0, top);
  const rest = ranked.slice(top);
  if (rest.length) {
    head.push({
      key: `${rest.length} other ${rest.length === 1 ? "model" : "models"}`,
      value: rest.reduce((sum, row) => sum + row.value, 0),
      requests: rest.reduce((sum, row) => sum + row.requests, 0),
    });
  }
  const total = ranked.reduce((sum, row) => sum + row.value, 0);
  const max = Math.max(0, ...head.map((row) => row.value));
  return head.map((row) => ({ ...row, share: max ? row.value / max : 0, percent: total ? row.value / total : 0 }));
}

export type { UsageDaily };
