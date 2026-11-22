// Token usage parts for views/UsageView.tsx: the daily bars, the price
// editor, request details and alerts.
import { Alert, Button, Modal, NumberInput, TextInput } from "@mantine/core";
import { useEffect, useMemo, useState } from "react";
import { formatGuestDateTime, guestDayKey, localDayKey } from "../../lib/time";
import { formatInteger, formatPrice, formatUsd } from "../../lib/format";
import {
  dayBars, formatMetric, metricOf, rangeStart, shortDay, type UsageMetric, type UsageRange,
} from "../../lib/usageView";
import { toIpcError } from "../../services/ipc/common";
import { usageIpc, type UsageDashboard, type UsagePrice } from "../../services/ipc/usage";
import type { PriceSourceStatus, UsageLog } from "../../services/ipc/generated/core";

const seconds = (ms: number | null) => (ms === null ? "—" : `${(ms / 1000).toFixed(1)}s`);

/** Daily bars with a readout that follows the pointer. */
export function DailyBars({ dashboard, metric, range }: { dashboard: UsageDashboard; metric: UsageMetric; range: UsageRange }) {
  const [hover, setHover] = useState<number | null>(null);
  const bars = useMemo(() => {
    const items = dashboard.daily.map((item) => ({ ...item, day: guestDayKey(item.timestamp) }));
    if (!items.length) return [];
    const days = items.map((item) => item.day).sort();
    const start = rangeStart(range);
    const first = start === null ? days[0] : localDayKey(start);
    return dayBars(items, (item) => metricOf(item, metric), first, localDayKey(Date.now()));
  }, [dashboard.daily, metric, range]);
  const max = Math.max(0, ...bars.map((bar) => bar.value));
  if (!bars.length || max === 0) return <div className="usage-empty">No activity in this period.</div>;
  const shown = hover != null ? bars[hover] : null;
  const total = bars.reduce((sum, bar) => sum + bar.value, 0);
  return (
    <div className="usage-bars-wrap">
      <div className="usage-readout" aria-live="polite">
        {shown ? (
          <><strong>{formatMetric(shown.value, metric)}</strong><span>{shortDay(shown.day)}{shown.item ? ` · ${formatInteger(shown.item.requests)} requests` : " · no activity"}</span></>
        ) : (
          <><strong>{formatMetric(total / bars.length, metric)}</strong><span>daily average</span></>
        )}
      </div>
      <div className="usage-bars" role="img" aria-label={`Daily ${metric}, ${shortDay(bars[0].day)} to ${shortDay(bars[bars.length - 1].day)}`} onPointerLeave={() => setHover(null)}>
        {bars.map((bar, index) => (
          <span key={bar.day} className={`usage-bar${hover === index ? " active" : ""}`} onPointerEnter={() => setHover(index)}>
            <span style={{ height: `${bar.value ? Math.max(2, (bar.value / max) * 100) : 0}%`, animationDelay: `${Math.min(index * 8, 400)}ms` }} />
          </span>
        ))}
      </div>
      <div className="usage-axis"><span>{shortDay(bars[0].day)}</span><span>{bars.length > 1 ? shortDay(bars[bars.length - 1].day) : ""}</span></div>
    </div>
  );
}

export function PriceEditor({ prices, sources, opened, onClose, onSaved }: { prices: UsagePrice[]; sources: PriceSourceStatus[]; opened: boolean; onClose: () => void; onSaved: () => void }) {
  const [editing, setEditing] = useState<UsagePrice | null>(null);
  const [search, setSearch] = useState("");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [values, setValues] = useState<{ model: string; input: number | string; output: number | string; cacheRead: number | string; cacheWrite: number | string }>({ model: "", input: "", output: "", cacheRead: "", cacheWrite: "" });
  useEffect(() => { if (!opened) setEditing(null); }, [opened]);

  const openEdit = (price?: UsagePrice) => {
    setEditing(price ?? { model: "", input: 0, output: 0, cacheRead: 0, cacheWrite: 0 });
    setValues(price ?? { model: "", input: "", output: "", cacheRead: "", cacheWrite: "" });
    setError(null);
  };
  const save = async () => {
    const fields = [values.input, values.output, values.cacheRead, values.cacheWrite];
    if (!values.model.trim() || fields.some((v) => v === "" || !Number.isFinite(Number(v)) || Number(v) < 0)) {
      setError("Enter a model name and a price for each token type.");
      return;
    }
    setSaving(true);
    setError(null);
    try {
      await usageIpc.savePrice({ model: values.model.trim(), input: Number(values.input), output: Number(values.output), cacheRead: Number(values.cacheRead), cacheWrite: Number(values.cacheWrite) });
      setEditing(null);
      onSaved();
    } catch (cause) { setError(toIpcError(cause).message); }
    finally { setSaving(false); }
  };
  const visible = prices.filter((price) => price.model.toLowerCase().includes(search.toLowerCase())).sort((a, b) => a.model.localeCompare(b.model));
  const source = sources[0];
  return (
    <Modal opened={opened} onClose={() => { if (!saving) onClose(); }} title={editing ? (editing.model ? "Edit price" : "Add a price") : "Model prices"} size="md" centered>
      {editing ? (
        <div className="usage-price-form">
          <TextInput label="Model" placeholder="Model ID" value={values.model} disabled={!!editing.model} onChange={(event) => setValues({ ...values, model: event.currentTarget.value })} />
          <div className="usage-price-fields">
            {([["input", "Input"], ["output", "Output"], ["cacheRead", "Cache read"], ["cacheWrite", "Cache write"]] as const).map(([key, label]) => (
              <NumberInput key={key} label={label} description="USD / 1M tokens" min={0} decimalScale={6} allowNegative={false} prefix="$" value={values[key]} onChange={(value) => setValues({ ...values, [key]: value })} />
            ))}
          </div>
          {error && <Alert color="red" role="alert">{error}</Alert>}
          <div className="usage-actions"><Button variant="default" disabled={saving} onClick={() => setEditing(null)}>Back</Button><Button loading={saving} onClick={() => void save()}>Save</Button></div>
        </div>
      ) : (
        <div className="usage-price-list-wrap">
          <div className="usage-price-source">
            <span>
              Prices come from models.dev{source?.lastSuccess != null ? ` · ${formatInteger(source.count)} models, updated ${formatGuestDateTime(source.lastSuccess)}` : " · not synced yet"}
              {source?.error && <span className="usage-bad"> · {source.error}</span>}
            </span>
          </div>
          {error && <Alert color="red" role="alert">{error}</Alert>}
          <div className="usage-price-head">
            <strong>Your overrides</strong>
            {prices.length > 6 && <TextInput aria-label="Search overrides" placeholder="Search" size="xs" value={search} onChange={(event) => setSearch(event.currentTarget.value)} />}
          </div>
          <div className="usage-price-list">
            {visible.map((price) => (
              <button key={price.model} className="usage-price-row" onClick={() => openEdit(price)}>
                <strong className="truncate">{price.model}</strong>
                <span>{formatPrice(price.input)} in · {formatPrice(price.output)} out</span>
              </button>
            ))}
            {!visible.length && <p className="usage-muted">{prices.length ? "No override matches." : "None. A model without any price counts as unpriced."}</p>}
          </div>
          <div className="usage-actions"><Button variant="default" onClick={() => openEdit()}>Add a price</Button></div>
        </div>
      )}
    </Modal>
  );
}

export function RequestDetail({ log, onClose }: { log: UsageLog | null; onClose: () => void }) {
  return (
    <Modal opened={log !== null} onClose={onClose} title="Request" size="sm" centered>
      {log && <dl className="usage-request-detail">
        {([
          ["Time", formatGuestDateTime(log.timestamp)],
          ["Agent", log.agent], ["Model", log.model], ["Provider", log.provider ?? "—"],
          ["Input", formatInteger(log.input)], ["Output", formatInteger(log.output)],
          ["Cache read", formatInteger(log.cacheRead)], ["Cache write", formatInteger(log.cacheWrite)],
          ["Cost", log.cost === null ? "Unpriced" : formatUsd(log.cost)],
          ["Duration", seconds(log.durationMs)],
          ["Status", log.status ?? "—"],
          ["Session", log.sessionId ?? "—"],
        ] as const).map(([label, value]) => <div key={label}><dt>{label}</dt><dd>{value}</dd></div>)}
      </dl>}
    </Modal>
  );
}

/** The usage problems worth showing once, above whichever report is open. */
export function UsageAlerts({ error, importErrors, onCloseError, onCloseImport }: {
  error: string | null;
  importErrors: string[];
  onCloseError: () => void;
  onCloseImport: () => void;
}) {
  return (
    <>
      {error && <Alert color="red" role="alert" withCloseButton onClose={onCloseError}>{error}</Alert>}
      {importErrors.length > 0 && (
        <Alert color="orange" role="status" withCloseButton onClose={onCloseImport} title="Some records could not be imported">
          {importErrors.slice(0, 3).map((message, index) => <div key={index}>{message}</div>)}
        </Alert>
      )}
    </>
  );
}
