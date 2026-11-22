/**
 * One time line for the Mac and the VM.
 *
 * Every timestamp from the guest (sessions, chat items, operations, quota
 * checks) is Unix milliseconds on the *guest's* clock, which can drift from
 * the Mac's (a VM paused while the Mac slept, no time sync on older
 * machines). The guest's time zone may differ too (the device profile sets
 * it on purpose), but that never matters here: timestamps carry no zone, and
 * the UI always shows them in the Mac's zone.
 *
 * Rule: guest timestamps are decoded for display through `lib/time.ts`, which
 * uses `hostTime` here. Use `hostTime` directly only for comparisons and
 * durations with `Date.now()`.
 * - `hostTime(guestMs)` puts a guest timestamp on the Mac's clock, for
 *   comparing it with `Date.now()` or feeding an age formatter.
 * - `guestNow()` is "now" on the guest's clock, for live timers measured
 *   against a guest start time.
 * Durations between two guest timestamps need neither.
 */
import { useSyncExternalStore } from "react";

/** One round trip: Mac time sent, guest time read, Mac time received. */
export interface ClockSample {
  sent: number;
  guest: number;
  received: number;
}

export interface ClockEstimate {
  /** Guest clock minus Mac clock, ms. */
  offset: number;
  /** Round trip of the sample the estimate came from, ms. */
  rtt: number;
}

/** Samples slower than this say little about the clock and are dropped. */
export const MAX_RTT_MS = 3000;
/** Smaller changes are noise from round-trip asymmetry and are ignored. */
export const MIN_CHANGE_MS = 500;

/** NTP-style estimate from the fastest round trip: the guest is assumed to
 * have read its clock halfway through it. Null when no sample is usable. */
export function estimateOffset(samples: ClockSample[]): ClockEstimate | null {
  let best: ClockSample | null = null;
  for (const sample of samples) {
    const rtt = sample.received - sample.sent;
    if (!Number.isFinite(sample.guest) || sample.guest <= 0 || rtt < 0 || rtt > MAX_RTT_MS) continue;
    if (!best || rtt < best.received - best.sent) best = sample;
  }
  if (!best) return null;
  return { offset: Math.round(best.guest - (best.sent + best.received) / 2), rtt: best.received - best.sent };
}

/** The offset to keep: the new estimate, unless it only moved by noise. */
export function nextOffset(current: number, estimate: ClockEstimate | null): number {
  if (!estimate) return current;
  return Math.abs(estimate.offset - current) < Math.max(MIN_CHANGE_MS, estimate.rtt / 2) ? current : estimate.offset;
}

export interface ClockState {
  /** Guest clock minus Mac clock, ms (0 until measured). */
  offset: number;
  /** The guest's time zone (IANA name), when known. */
  guestZone: string | null;
  /** When the offset was last measured (Mac clock), or null. */
  measuredAt: number | null;
}

let state: ClockState = { offset: 0, guestZone: null, measuredAt: null };
const listeners = new Set<() => void>();

export function clockState(): ClockState {
  return state;
}

/** Record a measurement; listeners run only when something visible changed. */
export function applyClock(estimate: ClockEstimate | null, guestZone: string | null, at = Date.now()) {
  const offset = nextOffset(state.offset, estimate);
  const zone = guestZone ?? state.guestZone;
  const changed = offset !== state.offset || zone !== state.guestZone;
  state = { offset, guestZone: zone, measuredAt: estimate ? at : state.measuredAt };
  if (changed) listeners.forEach((listener) => listener());
}

/** A guest timestamp on the Mac's clock. */
export function hostTime(guestMs: number): number {
  return guestMs - state.offset;
}

/** Now, on the guest's clock. */
export function guestNow(): number {
  return Date.now() + state.offset;
}

/** The Mac's time zone (IANA name), when the runtime reports one. */
export function hostZone(): string | null {
  try {
    return Intl.DateTimeFormat().resolvedOptions().timeZone || null;
  } catch {
    return null;
  }
}

/** Re-render when the offset or the guest's zone changes. */
export function useClock(): ClockState {
  return useSyncExternalStore(
    (listener) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    () => state,
  );
}

/** Below this the guest's clock counts as in step with the Mac's. */
export const SKEW_NOTICE_MS = 60 * 1000;

function skewText(ms: number): string {
  const seconds = Math.round(Math.abs(ms) / 1000);
  if (seconds < 120) return `${seconds} s`;
  const minutes = Math.round(seconds / 60);
  if (minutes < 120) return `${minutes} min`;
  return `${Math.round(minutes / 60)} h`;
}

/** What to tell the user about the two clocks, or null when they agree. */
export function clockNote(state: Pick<ClockState, "offset" | "guestZone">, macZone: string | null): string | null {
  const parts: string[] = [];
  if (state.guestZone && macZone && state.guestZone !== macZone) {
    parts.push(`The machine uses ${state.guestZone} and this Mac uses ${macZone}; the app shows every time in this Mac's zone.`);
  }
  if (Math.abs(state.offset) >= SKEW_NOTICE_MS) {
    parts.push(`The machine's clock is ${skewText(state.offset)} ${state.offset < 0 ? "behind" : "ahead of"} this Mac's; times shown here are corrected.`);
  }
  return parts.length ? parts.join(" ") : null;
}
