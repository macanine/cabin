/*
 * Number formatting is fixed to English digits and suffixes. Never format
 * counts with the system locale (`Intl.NumberFormat(undefined, …)`): a Mac
 * set to Chinese or Japanese turns 12,000 into "1.2万".
 */

const COUNT_UNITS = ["", "k", "M", "B", "T"] as const;

function trimFixed(value: number, digits: number): string {
  return value.toFixed(digits).replace(/\.0+$|(\.\d*?)0+$/, "$1");
}

/** 950 · 1.2k · 12k · 1.25M · 3.4B · 2T (k: one decimal below 10k; larger
 * units keep three significant digits). */
export function formatCount(value: number | null | undefined): string {
  if (value == null || !Number.isFinite(value)) return "—";
  const sign = value < 0 ? "-" : "";
  let rest = Math.abs(value);
  if (rest < 1000) return `${sign}${Math.round(rest)}`;
  let unit = 0;
  while (rest >= 1000 && unit < COUNT_UNITS.length - 1) {
    rest /= 1000;
    unit += 1;
  }
  const digits = unit === 1 ? (rest < 10 ? 1 : 0) : rest < 10 ? 2 : rest < 100 ? 1 : 0;
  let text = trimFixed(rest, digits);
  // 999.96k rounds to "1000k": carry into the next unit.
  if (Number(text) >= 1000 && unit < COUNT_UNITS.length - 1) {
    unit += 1;
    text = trimFixed(Number(text) / 1000, 2);
  }
  return `${sign}${text}${COUNT_UNITS[unit]}`;
}

const INTEGER = new Intl.NumberFormat("en-US", { maximumFractionDigits: 0 });

/** 12,345 — grouped, English digits. */
export function formatInteger(value: number | null | undefined): string {
  return value == null || !Number.isFinite(value) ? "—" : INTEGER.format(value);
}

/** $0 · $0.42 · $12.35 · $1,234 · $12.4k. Below a cent shows "<$0.01". */
export function formatUsd(value: number | null | undefined): string {
  if (value == null || !Number.isFinite(value)) return "—";
  if (value === 0) return "$0";
  const abs = Math.abs(value);
  const sign = value < 0 ? "-" : "";
  if (abs < 0.01) return `${sign}<$0.01`;
  if (abs < 1000) return `${sign}$${abs.toFixed(2)}`;
  if (abs < 10_000) return `${sign}$${INTEGER.format(abs)}`;
  return `${sign}$${formatCount(abs)}`;
}

/** A per-million-token price: $3 · $0.075 · $15.5 (up to 4 decimals). */
export function formatPrice(value: number | null | undefined): string {
  if (value == null || !Number.isFinite(value)) return "—";
  return `$${trimFixed(value, value < 1 ? 4 : 2)}`;
}

export function formatBytes(bytes: number | null | undefined): string {
  if (bytes == null) return "—";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value >= 10 || unit === 0 ? value.toFixed(0) : value.toFixed(1)} ${units[unit]}`;
}

export function formatRate(bytesPerSecond: number | null | undefined): string {
  return bytesPerSecond == null ? "—" : `${formatBytes(bytesPerSecond)}/s`;
}

export function formatPercent(value: number | null | undefined): string {
  return value == null ? "—" : `${Math.round(value)}%`;
}

export function formatMs(value: number | null | undefined): string {
  return value == null ? "—" : `${value} ms`;
}

export function relativeTime(at: number, now: number = Date.now()): string {
  const seconds = Math.max(0, Math.round((now - at) / 1000));
  if (seconds < 45) return "just now";
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `${minutes} min ago`;
  const hours = Math.round(minutes / 60);
  if (hours < 24) return `${hours} h ago`;
  const days = Math.round(hours / 24);
  return days === 1 ? "yesterday" : `${days} days ago`;
}

/** Points for an SVG polyline sparkline, newest on the right. */
export function sparkline(values: (number | null)[], width: number, height: number, max?: number): string {
  const points = values.map((value, index) => ({ value, index })).filter((p) => p.value != null) as {
    value: number;
    index: number;
  }[];
  if (points.length < 2) return "";
  const top = max ?? Math.max(...points.map((p) => p.value), 1);
  const step = width / Math.max(values.length - 1, 1);
  return points
    .map((p) => `${(p.index * step).toFixed(1)},${(height - (Math.min(p.value, top) / top) * height).toFixed(1)}`)
    .join(" ");
}

/** 3.2s · 42s · 1m 05s · 1h 3m */
export function formatDuration(ms: number | null | undefined): string {
  if (ms == null) return "";
  const seconds = Math.max(0, ms) / 1000;
  // One decimal below 10 s, without a trailing ".0" (1s, not 1.0s).
  if (seconds < 10) return `${Number(seconds.toFixed(1))}s`;
  if (seconds < 60) return `${Math.round(seconds)}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ${String(Math.floor(seconds % 60)).padStart(2, "0")}s`;
  return `${Math.floor(minutes / 60)}h ${minutes % 60}m`;
}

/** Percent (null when the size is unknown) and one line of text for a
 * running download: "312 MB of 610 MB · 12.4 MB/s · about 25s left". */
export function downloadSummary(d: {
  doneBytes: number;
  totalBytes: number | null;
  bytesPerSec: number | null;
  verifying: boolean;
}): { percent: number | null; text: string } {
  const percent = d.totalBytes ? Math.min(100, Math.floor((d.doneBytes / d.totalBytes) * 100)) : null;
  if (d.verifying) return { percent: 100, text: "Downloaded · verifying the SHA-256 checksum…" };
  const parts = [d.totalBytes ? `${formatBytes(d.doneBytes)} of ${formatBytes(d.totalBytes)}` : formatBytes(d.doneBytes)];
  if (d.bytesPerSec) parts.push(formatRate(d.bytesPerSec));
  if (d.bytesPerSec && d.totalBytes && d.totalBytes > d.doneBytes) {
    parts.push(`about ${formatDuration(((d.totalBytes - d.doneBytes) / d.bytesPerSec) * 1000)} left`);
  }
  return { percent, text: parts.join(" · ") };
}
