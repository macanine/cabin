/**
 * The one place that decodes timestamps.
 *
 * Every timestamp crossing IPC or stored by the backend is Unix milliseconds.
 * Two clocks exist:
 * - guest timestamps (sessions, chat, operations, usage) must go through
 *   `hostTime`/`guestDate`, which reads the measured offset in `clock.ts`;
 * - absolute timestamps (Mac events, server epochs such as Codex quota) are
 *   already on the Mac's clock and are formatted directly.
 *
 * Components never multiply by 1000 or call `new Date(guestTime)` themselves.
 */
import { hostTime } from "./clock.ts";
import { getLocale } from "../i18n/index.ts";

export const DAY_MS = 86_400_000;

/** Unix milliseconds. */
export type Millis = number;

export function absoluteDate(ms: Millis): Date {
  return new Date(ms);
}

/** Machine-readable serialization for `<time dateTime>`. */
export function isoTimestamp(ms: Millis): string {
  return absoluteDate(ms).toISOString();
}

export function guestDate(guestMs: Millis): Date {
  return new Date(hostTime(guestMs));
}

export function formatDateTime(
  ms: Millis,
  options: Intl.DateTimeFormatOptions = { dateStyle: "medium", timeStyle: "short" },
): string {
  return new Intl.DateTimeFormat(getLocale(), options).format(absoluteDate(ms));
}

export function formatDate(ms: Millis, options: Intl.DateTimeFormatOptions = { dateStyle: "medium" }): string {
  return new Intl.DateTimeFormat(getLocale(), options).format(absoluteDate(ms));
}

export function formatTime(ms: Millis, options: Intl.DateTimeFormatOptions = { timeStyle: "short" }): string {
  return new Intl.DateTimeFormat(getLocale(), options).format(absoluteDate(ms));
}

export function formatGuestDateTime(guestMs: Millis): string {
  return formatDateTime(hostTime(guestMs));
}

export function formatGuestDate(guestMs: Millis): string {
  return formatDate(hostTime(guestMs));
}

export function formatGuestTime(guestMs: Millis): string {
  return formatTime(hostTime(guestMs));
}

/** "YYYY-MM-DD" in the Mac's zone. */
export function localDayKey(ms: Millis): string {
  const date = absoluteDate(ms);
  const pad = (value: number) => String(value).padStart(2, "0");
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`;
}

export function guestDayKey(guestMs: Millis): string {
  return localDayKey(hostTime(guestMs));
}

/** Mac-local midnight of a `YYYY-MM-DD` key, or null when it is not a date. */
export function parseLocalDay(key: string): Millis | null {
  if (!/^\d{4}-\d{2}-\d{2}$/.test(key)) return null;
  const [year, month, day] = key.split("-").map(Number);
  const date = new Date(year, month - 1, day);
  return localDayKey(date.getTime()) === key ? date.getTime() : null;
}

/** Stable integer index of a day key (independent of the Mac's zone). */
export function localDayIndex(key: string): number | null {
  if (!/^\d{4}-\d{2}-\d{2}$/.test(key)) return null;
  const parsed = Date.parse(`${key}T00:00:00Z`);
  return Number.isFinite(parsed) ? Math.floor(parsed / DAY_MS) : null;
}

export function localDayKeyFromIndex(index: number): string {
  return new Date(index * DAY_MS).toISOString().slice(0, 10);
}

export function startOfLocalDay(ms: Millis): Millis {
  const date = absoluteDate(ms);
  date.setHours(0, 0, 0, 0);
  return date.getTime();
}

/** Monday = 0. */
export function weekdayIndex(dayKey: string): number {
  return (new Date(`${dayKey}T00:00:00Z`).getUTCDay() + 6) % 7;
}

/** Short clock label for a Mac-clock timestamp: "14:05", "Fri 14:05", "Jul 3 14:05". */
export function formatClockTime(ms: Millis, now: Millis = Date.now()): string {
  if (!ms) return "";
  const locale = getLocale();
  const date = absoluteDate(ms);
  const time = date.toLocaleTimeString(locale, { hour: "2-digit", minute: "2-digit" });
  if (localDayKey(ms) === localDayKey(now)) return time;
  if (now - ms < 6 * DAY_MS) {
    return `${date.toLocaleDateString(locale, { weekday: "short" })} ${time}`;
  }
  return `${date.toLocaleDateString(locale, { month: "short", day: "numeric" })} ${time}`;
}
