import { Popover, SegmentedControl, Tooltip } from "@mantine/core";
import { useEffect, useRef, useState } from "react";
import type { MonitorSample, ProcessInfo } from "../services/ipc/generated/core";
import { monitorIpc, type ProcessList } from "../services/ipc/monitor";
import { toIpcError } from "../services/ipc/common";
import { useApp } from "../state/store";
import { formatBytes, formatMs, formatRate, sparkline } from "../lib/format";
import { formatRunTime, gauges, gaugeSeries, type Gauge } from "../lib/perfGauges";

const R = 15;
const C = 2 * Math.PI * R;
const PROCESS_POLL_MS = 2000;

/** One ring: the share fills clockwise from 12 o'clock and eases to new values. */
function Ring({ gauge, size = 38, bare = false }: { gauge: Gauge; size?: number; bare?: boolean }) {
  const offset = gauge.percent == null ? C : C * (1 - gauge.percent / 100);
  return (
    <span className={`gauge-ring ${gauge.tone}`} style={{ width: size, height: size }}>
      <svg viewBox="0 0 36 36" aria-hidden>
        <circle className="gauge-ring-track" cx="18" cy="18" r={R} />
        <circle className="gauge-ring-fill" cx="18" cy="18" r={R} strokeDasharray={C} style={{ strokeDashoffset: offset }} />
      </svg>
      {!bare && <span className="gauge-ring-value">{gauge.center}</span>}
    </span>
  );
}

function Spark({ values, tone }: { values: (number | null)[]; tone: string }) {
  const points = sparkline(values, 100, 24, 100);
  return (
    <svg className={`perf-spark ${tone}`} viewBox="0 0 100 24" preserveAspectRatio="none" aria-hidden>
      {points && <polyline points={points} />}
    </svg>
  );
}

const io = (sample: MonitorSample | undefined) => [
  { label: "Disk", value: sample?.diskReadBps == null ? "—" : `${formatRate(sample.diskReadBps)} r · ${formatRate(sample.diskWriteBps ?? 0)} w` },
  { label: "Network", value: sample?.downloadBps == null ? "—" : `↓ ${formatRate(sample.downloadBps)} · ↑ ${formatRate(sample.uploadBps ?? 0)}` },
  { label: "Latency", value: formatMs(sample?.latencyMs) },
];

/** The busiest processes, polled every 2 s while the details are open. */
function useProcesses(open: boolean) {
  const [list, setList] = useState<ProcessList | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    if (!open) return;
    let active = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const load = async () => {
      try {
        const next = await monitorIpc.processes(8);
        if (active) { setList(next); setError(null); }
      } catch (cause) {
        if (active) setError(toIpcError(cause).message);
      } finally {
        if (active) timer = setTimeout(() => void load(), PROCESS_POLL_MS);
      }
    };
    void load();
    return () => { active = false; clearTimeout(timer); };
  }, [open]);
  return { list, error };
}

function ProcessRow({ process, sort, max }: { process: ProcessInfo; sort: "cpu" | "memory"; max: number }) {
  const value = sort === "cpu" ? process.cpuPercent : process.memory;
  const share = max > 0 ? Math.max(2, (value / max) * 100) : 0;
  return (
    <li className="proc" title={process.command || process.name}>
      <span className="proc-bar" style={{ width: `${share}%` }} aria-hidden />
      <span className="proc-main">
        <span className="proc-name">
          <strong>{process.name}</strong>
          <span className="proc-pid">{process.pid}</span>
        </span>
        <span className="proc-cmd">{process.command || "kernel thread"}</span>
      </span>
      <span className="proc-figures">
        <strong>{sort === "cpu" ? `${process.cpuPercent.toFixed(process.cpuPercent >= 10 ? 0 : 1)}%` : formatBytes(process.memory)}</strong>
        <small>{sort === "cpu" ? formatBytes(process.memory) : `${process.cpuPercent.toFixed(1)}%`} · {formatRunTime(process.runTime)}</small>
      </span>
    </li>
  );
}

function Details({ open }: { open: boolean }) {
  const history = useApp((state) => state.monitor);
  const target = useApp((state) => state.settings?.latencyTarget ?? "");
  const sample = history[history.length - 1];
  const [sort, setSort] = useState<"cpu" | "memory">("cpu");
  const { list, error } = useProcesses(open);
  const rows = list ? (sort === "cpu" ? list.byCpu : list.byMemory) : [];
  const max = rows.reduce((top, row) => Math.max(top, sort === "cpu" ? row.cpuPercent : row.memory), 0);

  return (
    <div className="perf-details">
      <header className="perf-details-head">
        <span className="perf-live" aria-hidden />
        <strong>Machine</strong>
        <span className="faint">{sample?.cpuCount ? `${sample.cpuCount} cores · ` : ""}live, every second</span>
      </header>

      <ul className="perf-metrics">
        {gauges(sample).map((gauge) => (
          <li key={gauge.key}>
            <Ring gauge={gauge} size={30} />
            <span className="perf-metric-copy">
              <strong>{gauge.label}</strong>
              <span>{gauge.detail}</span>
            </span>
            <Spark values={gaugeSeries(history, gauge.key)} tone={gauge.tone} />
          </li>
        ))}
      </ul>

      <div className="perf-io">
        {io(sample).map((entry) => (
          <span key={entry.label} title={entry.label === "Latency" && target ? `TCP connect to ${target}` : undefined}>
            <small>{entry.label}</small>
            <strong>{entry.value}</strong>
          </span>
        ))}
      </div>

      <section className="perf-procs">
        <div className="perf-procs-head">
          <strong>Processes</strong>
          {list && <span className="faint">{list.total} running</span>}
          <span className="grow" />
          <SegmentedControl size="xs" value={sort} onChange={(value) => setSort(value as "cpu" | "memory")}
            data={[{ value: "cpu", label: "CPU" }, { value: "memory", label: "Memory" }]} aria-label="Sort processes" />
        </div>
        {error ? <p className="perf-procs-empty bad">{error}</p>
          : !list ? <ul className="perf-proc-list">{Array.from({ length: 5 }, (_, i) => <li key={i} className="proc skeleton" />)}</ul>
            : (
              <ul className="perf-proc-list" key={sort}>
                {rows.map((process) => <ProcessRow key={process.pid} process={process} sort={sort} max={max} />)}
              </ul>
            )}
      </section>
    </div>
  );
}

/**
 * The machine at a glance: four rings (CPU, RAM, swap, load per core) in one
 * short row. Clicking opens the details beside the sidebar: each metric with
 * its real numbers and last minute, I/O and latency, and the busiest
 * processes (CPU or memory), refreshed while open.
 */
export function PerformancePanel() {
  const history = useApp((state) => state.monitor);
  const [open, setOpen] = useState(false);
  const sample = history[history.length - 1];
  const button = useRef<HTMLButtonElement>(null);

  return (
    <Popover
      opened={open}
      onChange={setOpen}
      position="right-end"
      offset={14}
      width={392}
      radius="lg"
      withinPortal
      trapFocus={false}
      returnFocus
    >
      <Popover.Target>
        {/* Controlled: Popover.Target adds no click handler, the button toggles. */}
        <button ref={button} type="button" className={`press grid min-w-0 cursor-pointer grid-cols-4 gap-1 rounded-b-[var(--card-radius)] border-0 border-t border-solid border-line/60 px-2 py-1.5 text-fg ${open ? "bg-hover" : "bg-transparent hover:bg-hover"}`} onClick={() => setOpen((value) => !value)}
          aria-label="Machine performance" aria-expanded={open}>
          {gauges(sample).map((gauge) => (
            <Tooltip key={gauge.key} label={`${gauge.label}: ${gauge.detail}`} openDelay={400} withinPortal disabled={open}>
              <span className="flex min-w-0 items-center gap-1.5">
                <Ring gauge={gauge} size={20} bare />
                <span className="grid min-w-0 text-left leading-tight">
                  <span className="text-[9.5px] font-medium text-faint">{gauge.label}</span>
                  <span className="truncate text-[11px] font-semibold tabular-nums">{gauge.center}</span>
                </span>
              </span>
            </Tooltip>
          ))}
        </button>
      </Popover.Target>
      <Popover.Dropdown className="perf-dropdown">
        <Details open={open} />
      </Popover.Dropdown>
    </Popover>
  );
}
