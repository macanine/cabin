import type { StartPhase, StartReport } from "../services/ipc/generated/core";

/** Time a phase took, or has taken so far while it runs. */
export function phaseElapsed(phase: StartPhase, now: number): number | null {
  if (phase.startedAt == null) return null;
  return (phase.finishedAt ?? (phase.state === "running" ? now : phase.startedAt)) - phase.startedAt;
}

export function reportElapsed(report: StartReport, now: number): number {
  return (report.finishedAt ?? now) - report.startedAt;
}

/** The phase to point at: the failed one, else the running one. */
export function focusPhase(report: StartReport): StartPhase | null {
  return report.phases.find((p) => p.state === "failed") ?? report.phases.find((p) => p.state === "running") ?? null;
}

/**
 * The phases worth showing: those that have begun. Pending phases have not
 * happened yet and skipped ones did not happen at all (attaching to a running
 * machine skips the boot), so each step appears as the start reaches it.
 */
export function visiblePhases(report: StartReport): StartPhase[] {
  return report.phases.filter((p) => p.state !== "pending" && p.state !== "skipped");
}

/** Phases still ahead of the current one. */
export function remainingPhases(report: StartReport): number {
  return report.state === "running" ? report.phases.filter((p) => p.state === "pending").length : 0;
}

/**
 * Overall progress in [0, 1]: settled phases count fully, the running one
 * half. Skipped phases count as settled, so attaching jumps ahead honestly.
 */
export function startProgress(report: StartReport): number {
  if (report.state === "succeeded") return 1;
  const total = report.phases.length;
  if (total === 0) return 0;
  const units = report.phases.reduce(
    (sum, p) => sum + (p.state === "done" || p.state === "skipped" || p.state === "failed" ? 1 : p.state === "running" ? 0.5 : 0),
    0,
  );
  return Math.min(1, units / total);
}

/** True when the start found QEMU already running and only reconnected. */
export function isAttach(report: StartReport): boolean {
  return report.phases.some((p) => p.id === "boot" && p.state === "skipped");
}

/** `[+12.345s] text` → `text`; the elapsed prefix is noise in a one-line ticker. */
const ELAPSED = /^\[\+\d+(?:\.\d+)?s\]\s*/;

/**
 * The newest meaningful line of the start log, shown under the running step
 * so the user sees motion without reading a log pane.
 */
export function logTicker(lines: string[]): string | null {
  for (let i = lines.length - 1; i >= 0; i--) {
    const line = lines[i].replace(ELAPSED, "").trim();
    if (line) return line.length > 140 ? `${line.slice(0, 139)}…` : line;
  }
  return null;
}
