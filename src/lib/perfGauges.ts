// The sidebar's performance rings (components/MonitorPanel.tsx): CPU, RAM,
// swap and load, each as a share of what the machine has.
import type { MonitorSample } from "../services/ipc/generated/core";
import { formatBytes } from "./format.ts";

export type GaugeTone = "ok" | "warn" | "bad" | "off";
export type GaugeKey = "cpu" | "ram" | "swap" | "load";

export interface Gauge {
  key: GaugeKey;
  label: string;
  /** 0–100 for the ring, or null when unknown. */
  percent: number | null;
  /** The short figure inside the ring. */
  center: string;
  /** One line with the real numbers, for the details and tooltips. */
  detail: string;
  tone: GaugeTone;
}

/** Calm below 70%, a warning from 70%, a problem from 90%. */
export function toneOf(percent: number | null): GaugeTone {
  if (percent == null) return "off";
  return percent >= 90 ? "bad" : percent >= 70 ? "warn" : "ok";
}

const clamp = (value: number) => Math.max(0, Math.min(100, value));
const share = (used: number | null | undefined, total: number | null | undefined) =>
  used != null && total ? clamp((used / total) * 100) : null;
const whole = (percent: number | null) => (percent == null ? "—" : `${Math.round(percent)}%`);

export function gauges(sample: MonitorSample | undefined): Gauge[] {
  const cpu = sample?.cpuPercent ?? null;
  const ram = share(sample?.memoryUsed, sample?.memoryTotal);
  const swap = share(sample?.swapUsed, sample?.swapTotal);
  const load = sample?.load ?? null;
  const cores = sample?.cpuCount ?? null;
  // Load per core: 1.0 on every core is "full".
  const loadPercent = load && cores ? clamp((load.one / cores) * 100) : null;
  return [
    {
      key: "cpu", label: "CPU", percent: cpu == null ? null : clamp(cpu), center: whole(cpu),
      detail: cpu == null ? "Not available" : `${Math.round(cpu)}% of ${cores ?? "?"} core${cores === 1 ? "" : "s"}`,
      tone: toneOf(cpu),
    },
    {
      key: "ram", label: "RAM", percent: ram, center: whole(ram),
      detail: sample?.memoryUsed == null ? "Not available" : `${formatBytes(sample.memoryUsed)} of ${formatBytes(sample.memoryTotal)}`,
      tone: toneOf(ram),
    },
    {
      key: "swap", label: "Swap", percent: swap, center: sample?.swapTotal ? whole(swap) : "off",
      detail: sample?.swapTotal ? `${formatBytes(sample.swapUsed)} of ${formatBytes(sample.swapTotal)}` : "No swap in the machine",
      tone: sample?.swapTotal ? toneOf(swap) : "off",
    },
    {
      key: "load", label: "Load", percent: loadPercent, center: load ? load.one.toFixed(load.one >= 10 ? 0 : 1) : "—",
      detail: load ? `${load.one.toFixed(2)} · ${load.five.toFixed(2)} · ${load.fifteen.toFixed(2)} (1, 5, 15 min)` : "Not available",
      tone: toneOf(loadPercent),
    },
  ];
}

/** The series of one gauge over the samples, for the sparklines. */
export function gaugeSeries(history: readonly MonitorSample[], key: GaugeKey): (number | null)[] {
  return history.map((sample) => gauges(sample).find((gauge) => gauge.key === key)?.percent ?? null);
}

/** "45s", "12m", "3h 05m", "4d 2h". */
export function formatRunTime(seconds: number): string {
  if (seconds < 60) return `${Math.max(0, Math.floor(seconds))}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h ${String(minutes % 60).padStart(2, "0")}m`;
  return `${Math.floor(hours / 24)}d ${hours % 24}h`;
}
