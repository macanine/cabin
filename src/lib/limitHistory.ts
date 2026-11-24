// Readings of one plan window over time, so the limits can be drawn as a
// burn line rather than a single bar. Providers report only the current
// share, so Cabin keeps each reading it sees (one per refresh), keyed by the
// account, the window and its reset: a new reset starts a new line. The
// history is a viewing aid kept in this window's storage; losing it only
// shortens the drawn line.

export interface LimitSample {
  /** Unix ms of the reading. */
  t: number;
  /** Share used, 0–100. */
  used: number;
}

interface Store {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

const KEY = "cabin.limitHistory.v1";
const MAX_SAMPLES = 240;
/** Readings closer than this with the same value add nothing. */
const MIN_GAP_MS = 60_000;

type History = Record<string, { resetsAt: number; samples: LimitSample[] }>;

function storage(): Store | null {
  try { return typeof localStorage === "undefined" ? null : localStorage; } catch { return null; }
}

function load(store: Store | null): History {
  try { return JSON.parse(store?.getItem(KEY) ?? "{}") as History; } catch { return {}; }
}

/** One line per account and window; a different reset is a different window. */
export function historyKey(scope: string, label: string, resetsAt: number): string {
  // Providers move a reset by seconds between reads; minutes identify the window.
  return `${scope}|${label}|${Math.round(resetsAt / 60_000)}`;
}

/** Add a reading; drop finished windows. Returns the window's samples. */
export function recordSample(key: string, resetsAt: number, sample: LimitSample, now = Date.now(), store: Store | null = storage()): LimitSample[] {
  const history = load(store);
  for (const [k, entry] of Object.entries(history)) if (entry.resetsAt < now) delete history[k];
  const entry = history[key] ?? { resetsAt, samples: [] };
  const last = entry.samples[entry.samples.length - 1];
  const used = Math.max(0, Math.min(100, sample.used));
  const stale = last != null && sample.t <= last.t;
  // An unchanged value is worth a point only every ten minutes (to keep a flat stretch flat).
  const redundant = last != null && used === last.used && sample.t - last.t < MIN_GAP_MS * 10;
  if (!stale && !redundant) {
    // Two reads within a minute are one reading: keep the newer.
    if (last && sample.t - last.t < MIN_GAP_MS) entry.samples[entry.samples.length - 1] = { t: sample.t, used };
    else entry.samples.push({ t: sample.t, used });
  }
  entry.samples = entry.samples.slice(-MAX_SAMPLES);
  history[key] = entry;
  try { store?.setItem(KEY, JSON.stringify(history)); } catch { /* full or blocked: keep drawing from memory */ }
  return entry.samples;
}

export function samplesFor(key: string, store: Store | null = storage()): LimitSample[] {
  return load(store)[key]?.samples ?? [];
}

export interface BurnGeometry {
  /** Usage so far, 0–1 on both axes (x: window time, y: share used). */
  actual: [number, number][];
  /** From the last reading to the reset (or to 100% when it runs out first). */
  projection: [number, number][];
  /** Where the line meets 100% before the reset, if it does. */
  exhaust: [number, number] | null;
  now: number;
}

/**
 * The burn line for a window that opened at `reset - span`: it starts at 0%
 * (a window opens empty), passes through every reading, and continues at the
 * pace since the window opened.
 */
export function burnGeometry(samples: readonly LimitSample[], current: LimitSample, resetsAt: number, span: number): BurnGeometry {
  const start = resetsAt - span;
  const x = (t: number) => Math.max(0, Math.min(1, (t - start) / span));
  const y = (used: number) => Math.max(0, Math.min(1, used / 100));
  const points = [...samples.filter((s) => s.t >= start && s.t < current.t), current];
  const actual: [number, number][] = [[0, 0], ...points.map((s) => [x(s.t), y(s.used)] as [number, number])];
  const nowX = x(current.t);
  const spent = current.t - start;
  let projection: [number, number][] = [];
  let exhaust: [number, number] | null = null;
  if (spent > 0 && current.used > 0 && current.used < 100 && nowX < 1) {
    const rate = current.used / spent;
    const landing = current.used + rate * (resetsAt - current.t);
    if (landing <= 100) projection = [[nowX, y(current.used)], [1, y(landing)]];
    else {
      const outAt = current.t + (100 - current.used) / rate;
      exhaust = [x(outAt), 1];
      projection = [[nowX, y(current.used)], exhaust, [1, 1]];
    }
  }
  return { actual, projection, exhaust, now: nowX };
}
