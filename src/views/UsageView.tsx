import { ActionIcon, Button, Loader, Menu, SegmentedControl, Select } from "@mantine/core";
import { IconCoin, IconDots } from "@tabler/icons-react";
import { useEffect, useMemo, useState } from "react";
import { AgentChips, isAgentKind } from "../components/AgentPicker";
import { AGENT_NAMES } from "../components/common";
import { MachineGate } from "../components/MachineGate";
import { useUsage } from "../components/usage/UsageMonitor";
import { ViewHeader } from "../components/TitleBar";
import {
  DailyBars, PriceEditor, RequestDetail, UsageAlerts,
} from "../components/usage/usageParts";
import { hostZone, useClock } from "../lib/clock";
import { formatCount, formatInteger, formatPrice, formatUsd } from "../lib/format";
import { formatGuestDateTime, formatTime } from "../lib/time";
import {
  cacheHitRate, formatMetric, modelShares, rangeStart, totalTokens, USAGE_RANGES,
  type UsageMetric, type UsageRange,
} from "../lib/usageView";
import { toIpcError } from "../services/ipc/common";
import type { AgentKind, UsageLog } from "../services/ipc/generated/core";
import { usageIpc, type UsageDashboard, type UsageFilter } from "../services/ipc/usage";

const PAGE_SIZE = 20;
const RANGE_KEY = "cabin.usageRange";
const METRICS: { value: UsageMetric; label: string }[] = [
  { value: "cost", label: "Cost" },
  { value: "tokens", label: "Tokens" },
  { value: "requests", label: "Requests" },
];
type Tab = "requests" | "models" | "agents" | "pricing";
const TABS: { value: Tab; label: string }[] = [
  { value: "requests", label: "Requests" },
  { value: "models", label: "Models" },
  { value: "agents", label: "Agents" },
  { value: "pricing", label: "Pricing" },
];

function readRange(): UsageRange {
  try {
    const saved = localStorage.getItem(RANGE_KEY);
    return USAGE_RANGES.some((range) => range.value === saved) ? (saved as UsageRange) : "30";
  } catch {
    return "30";
  }
}

/** The period picker spells the ranges out. */
const RANGE_NAMES: Record<UsageRange, string> = { "1": "Today", "7": "Last 7 days", "30": "Last 30 days", "90": "Last 90 days", all: "All time" };
const RANGE_OPTIONS = USAGE_RANGES.map((entry) => ({ value: entry.value, label: RANGE_NAMES[entry.value] }));

const agentName = (agent: string) => (isAgentKind(agent) ? AGENT_NAMES[agent] : agent);

/**
 * Token usage, on its own page: a filter row (agent, model, period), the
 * total with its daily trend beside what it is made of, then the details as tables (requests, models,
 * agents, pricing). The app's usage monitor imports records in the background.
 */
export function UsageView() {
  const usage = useUsage();
  const [pricesOpen, setPricesOpen] = useState(false);
  const [dashboard, setDashboard] = useState<UsageDashboard | null>(null);
  return (
    <>
      <ViewHeader title="Usage" subtitle={usage.syncedAt != null ? `Synced ${formatTime(usage.syncedAt)}` : "Syncing agent records…"}>
        <Menu position="bottom-end" withinPortal>
          <Menu.Target><ActionIcon variant="subtle" color="gray" aria-label="More"><IconDots size={16} /></ActionIcon></Menu.Target>
          <Menu.Dropdown>
            <Menu.Item leftSection={<IconCoin size={14} />} onClick={() => setPricesOpen(true)}>Model prices…</Menu.Item>
          </Menu.Dropdown>
        </Menu>
      </ViewHeader>
      <MachineGate>
        <UsageContent usage={usage} onDashboard={setDashboard} onEditPrices={() => setPricesOpen(true)} />
      </MachineGate>
      <PriceEditor prices={dashboard?.prices ?? []} sources={dashboard?.priceSources ?? []} opened={pricesOpen}
        onClose={() => setPricesOpen(false)} onSaved={usage.bump} />
    </>
  );
}

function UsageContent({ usage, onDashboard, onEditPrices }: {
  usage: ReturnType<typeof useUsage>;
  onDashboard: (dashboard: UsageDashboard) => void;
  onEditPrices: () => void;
}) {
  const { offset: clockOffset } = useClock();
  const zone = hostZone();
  const [range, setRangeState] = useState<UsageRange>(readRange);
  const [agent, setAgent] = useState<AgentKind | null>(null);
  const [model, setModel] = useState<string | null>(null);
  const [metric, setMetric] = useState<UsageMetric>("cost");
  const [tab, setTab] = useState<Tab>("requests");
  const [page, setPage] = useState(0);
  const [dashboard, setDashboard] = useState<UsageDashboard | null>(null);
  const [modelOptions, setModelOptions] = useState<string[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [selectedLog, setSelectedLog] = useState<UsageLog | null>(null);
  const start = rangeStart(range);
  // The guest stores its own clock; shift the Mac-local boundary onto it.
  const fromMs = start === null ? null : start + clockOffset;

  const setRange = (value: UsageRange) => {
    setRangeState(value);
    setPage(0);
    try { localStorage.setItem(RANGE_KEY, value); } catch { /* not remembered */ }
  };

  useEffect(() => {
    let current = true;
    if (!dashboard) setLoading(true);
    const filter: UsageFilter = { fromMs, toMs: null, agent, model, limit: PAGE_SIZE, offset: page * PAGE_SIZE, hostZone: zone, clockOffset };
    usageIpc.dashboard(filter).then((result) => {
      if (!current) return;
      if (page > 0 && page * PAGE_SIZE >= result.total) {
        setPage(Math.max(0, Math.ceil(result.total / PAGE_SIZE) - 1));
        return;
      }
      setDashboard(result);
      onDashboard(result);
      setError(null);
      if (model === null) setModelOptions(result.models.map((item) => item.model));
    }).catch((cause) => { if (current) setError(toIpcError(cause).message); })
      .finally(() => { if (current) setLoading(false); });
    return () => { current = false; };
    // `dashboard` is this effect's own output; including it would refetch on
    // every result, so the request keys are the dependencies.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [fromMs, agent, model, page, usage.revision, zone, clockOffset, onDashboard]);

  const summary = dashboard?.summary;
  const hitRate = summary ? cacheHitRate(summary) : null;
  const options = [...new Set([...modelOptions, ...(model ? [model] : [])])].sort();
  const pageCount = Math.max(1, Math.ceil((dashboard?.total ?? 0) / PAGE_SIZE));
  const shares = useMemo(() => (dashboard ? modelShares(dashboard.models, metric, 50) : []), [dashboard, metric]);
  const agents = useMemo(() => (dashboard?.agents ?? []).slice().sort((a, b) => b.cost - a.cost), [dashboard]);

  return (
    <div className="usage-page">
      <div className="usage-inner">
        <div className="usage-filters">
          <AgentChips value={agent} onChange={(next) => { setAgent(next); setModel(null); setPage(0); }} />
          <span className="grow" />
          {options.length > 1 && (
            <Select aria-label="Model" placeholder="All models" size="xs" clearable searchable value={model} data={options}
              onChange={(value) => { setModel(value); setPage(0); }} className="usage-model-filter" />
          )}
          <Select aria-label="Period" size="xs" value={range} data={RANGE_OPTIONS} allowDeselect={false}
            onChange={(value) => value && setRange(value as UsageRange)} className="usage-range" />
        </div>

        <UsageAlerts error={usage.error ?? error} importErrors={usage.importErrors}
          onCloseError={() => { usage.clearError(); setError(null); }} onCloseImport={usage.clearImportErrors} />

        {!dashboard || !summary ? (
          <div className="usage-loading">{loading ? <><Loader size="sm" /> Loading usage…</> : "No usage data."}</div>
        ) : (
          <div className={`usage-results${loading ? " loading" : ""}`} aria-busy={loading}>
            <div className="usage-top">
              <section className="usage-card usage-hero" aria-label="Summary">
                <div className="usage-card-head">
                  <div className="usage-headline">
                    <span>Total cost · {RANGE_NAMES[range]}</span>
                    <strong>{formatUsd(summary.cost)}</strong>
                    {summary.unpricedRequests > 0 && <small>+ {formatInteger(summary.unpricedRequests)} unpriced requests</small>}
                  </div>
                  <SegmentedControl size="xs" value={metric} data={METRICS} onChange={(value) => setMetric(value as UsageMetric)} aria-label="Measure" />
                </div>
                <DailyBars dashboard={dashboard} metric={metric} range={range} />
                <div className="usage-figures">
                  <Stat label="Requests" value={formatInteger(summary.requests)} />
                  <Stat label="Tokens" value={formatCount(totalTokens(summary))} note={`${formatCount(summary.input)} in · ${formatCount(summary.output)} out`} />
                  <Stat label="Cache hit rate" value={hitRate === null ? "—" : `${(hitRate * 100).toFixed(1)}%`} note={`${formatCount(summary.cacheRead)} from cache`} />
                </div>
              </section>
              <aside className="usage-rail">
                <ShareCard title="By agent" rows={agents.map((row) => ({ key: row.agent, label: agentName(row.agent), cost: row.cost }))} active={agent}
                  onPick={(key) => { if (isAgentKind(key)) { setAgent(agent === key ? null : key); setModel(null); setPage(0); } }} />
                <ShareCard title="Top models" mono rows={dashboard.models.map((row) => ({ key: row.model, label: row.model, cost: row.cost }))} active={model}
                  onPick={(key) => { setModel(model === key ? null : key); setPage(0); }} />
              </aside>
            </div>

            <section className="usage-card usage-tables">
              <div className="usage-tabs" role="tablist" aria-label="Details">
                {TABS.map((entry) => (
                  <button key={entry.value} type="button" role="tab" aria-selected={tab === entry.value} onClick={() => setTab(entry.value)}>
                    {entry.label}
                    {entry.value === "requests" && <span className="usage-tab-count">{formatCount(dashboard.total)}</span>}
                  </button>
                ))}
              </div>

              {tab === "requests" && (dashboard.logs.length ? (
                <>
                  <div className="usage-table-wrap">
                    <table className="usage-table">
                      <thead>
                        <tr><th>Time</th><th>Agent</th><th>Model</th><th className="num">Input</th><th className="num">Output</th><th className="num">Cache read</th><th className="num">Cost</th></tr>
                      </thead>
                      <tbody>
                        {dashboard.logs.map((log) => (
                          <tr key={log.id} tabIndex={0} onClick={() => setSelectedLog(log)} onKeyDown={(event) => event.key === "Enter" && setSelectedLog(log)}>
                            <td className="nowrap">{formatGuestDateTime(log.timestamp)}</td>
                            <td>{agentName(log.agent)}</td>
                            <td className="mono usage-model-cell" title={log.model}>{log.model}</td>
                            <td className="num">{formatCount(log.input)}</td>
                            <td className="num">{formatCount(log.output)}</td>
                            <td className="num">{formatCount(log.cacheRead)}</td>
                            <td className="num strong">{log.cost === null ? "—" : formatUsd(log.cost)}</td>
                          </tr>
                        ))}
                      </tbody>
                    </table>
                  </div>
                  {pageCount > 1 && (
                    <div className="usage-pager">
                      <Button size="compact-xs" variant="subtle" color="gray" disabled={loading || page === 0} onClick={() => setPage(page - 1)}>Newer</Button>
                      <span>{page + 1} / {pageCount}</span>
                      <Button size="compact-xs" variant="subtle" color="gray" disabled={loading || page + 1 >= pageCount} onClick={() => setPage(page + 1)}>Older</Button>
                    </div>
                  )}
                </>
              ) : <div className="usage-empty">No requests in this period.</div>)}

              {tab === "models" && (shares.length ? (
                <div className="usage-table-wrap">
                  <table className="usage-table">
                    <thead><tr><th>Model</th><th className="num">Requests</th><th className="num">Tokens</th><th className="num">Cost</th><th className="usage-share-col">Share of {metric}</th></tr></thead>
                    <tbody>
                      {shares.map((row) => {
                        const item = dashboard.models.find((entry) => entry.model === row.key);
                        return (
                          <tr key={row.key}>
                            <td className="mono usage-model-cell" title={row.key}>{row.key}</td>
                            <td className="num">{formatInteger(row.requests)}</td>
                            <td className="num">{item ? formatCount(totalTokens(item)) : "—"}</td>
                            <td className="num strong">{item ? formatUsd(item.cost) : formatMetric(row.value, metric)}</td>
                            <td className="usage-share-col">
                              <span className="usage-share-track"><span style={{ width: `${row.share * 100}%` }} /></span>
                              <small>{Math.round(row.percent * 100)}%</small>
                            </td>
                          </tr>
                        );
                      })}
                    </tbody>
                  </table>
                </div>
              ) : <div className="usage-empty">No models in this period.</div>)}

              {tab === "agents" && (agents.length ? (
                <div className="usage-table-wrap">
                  <table className="usage-table">
                    <thead><tr><th>Agent</th><th className="num">Requests</th><th className="num">Tokens</th><th className="num">Cache hit</th><th className="num">Cost</th></tr></thead>
                    <tbody>
                      {agents.map((row) => {
                        const rate = cacheHitRate(row);
                        return (
                          <tr key={row.agent} tabIndex={0} onClick={() => isAgentKind(row.agent) && setAgent(row.agent)}>
                            <td>{agentName(row.agent)}</td>
                            <td className="num">{formatInteger(row.requests)}</td>
                            <td className="num">{formatCount(totalTokens(row))}</td>
                            <td className="num">{rate === null ? "—" : `${Math.round(rate * 100)}%`}</td>
                            <td className="num strong">{formatUsd(row.cost)}</td>
                          </tr>
                        );
                      })}
                    </tbody>
                  </table>
                </div>
              ) : <div className="usage-empty">No agent activity in this period.</div>)}

              {tab === "pricing" && (
                <div className="usage-pricing">
                  <p className="usage-muted">
                    Costs use agent-reported amounts when available. Otherwise they show API-equivalent estimates from models.dev
                    {dashboard.priceSources[0]?.lastSuccess != null ? ` (${formatInteger(dashboard.priceSources[0].count)} models, updated ${formatGuestDateTime(dashboard.priceSources[0].lastSuccess)})` : ""}.
                    Your overrides win. Unpriced requests still count toward usage.
                  </p>
                  {dashboard.prices.length ? (
                    <div className="usage-table-wrap">
                      <table className="usage-table">
                        <thead><tr><th>Model</th><th className="num">Input</th><th className="num">Output</th><th className="num">Cache read</th><th className="num">Cache write</th></tr></thead>
                        <tbody>
                          {dashboard.prices.slice().sort((a, b) => a.model.localeCompare(b.model)).map((price) => (
                            <tr key={price.model}>
                              <td className="mono usage-model-cell">{price.model}</td>
                              <td className="num">{formatPrice(price.input)}</td>
                              <td className="num">{formatPrice(price.output)}</td>
                              <td className="num">{formatPrice(price.cacheRead)}</td>
                              <td className="num">{formatPrice(price.cacheWrite)}</td>
                            </tr>
                          ))}
                        </tbody>
                      </table>
                    </div>
                  ) : <div className="usage-empty">No price overrides. Prices per 1M tokens.</div>}
                  <div><Button size="xs" variant="default" leftSection={<IconCoin size={14} />} onClick={onEditPrices}>Edit prices…</Button></div>
                </div>
              )}
            </section>
          </div>
        )}
      </div>
      <RequestDetail log={selectedLog} onClose={() => setSelectedLog(null)} />
    </div>
  );
}

/** What the cost is made of, largest first; a row filters the page by it. */
function ShareCard({ title, rows, active, onPick, mono = false }: {
  title: string;
  rows: { key: string; label: string; cost: number }[];
  active: string | null;
  onPick: (key: string) => void;
  mono?: boolean;
}) {
  const total = rows.reduce((sum, row) => sum + row.cost, 0);
  const shown = rows.slice().sort((a, b) => b.cost - a.cost).slice(0, 5);
  return (
    <section className="usage-card" aria-label={title}>
      <h2 className="usage-rail-title">{title}</h2>
      {shown.length === 0 && <p className="usage-muted m-0">Nothing in this period.</p>}
      {shown.map((row) => (
        <button key={row.key} type="button" className="usage-share press" aria-pressed={active === row.key} title={row.label} onClick={() => onPick(row.key)}>
          <span className={`truncate ${mono ? "font-mono text-xs" : ""}`}>{row.label}</span>
          <strong>{formatUsd(row.cost)}</strong>
          <span className="usage-share-bar" aria-hidden><span style={{ inlineSize: `${total > 0 ? (row.cost / total) * 100 : 0}%` }} /></span>
        </button>
      ))}
    </section>
  );
}

function Stat({ label, value, note }: { label: string; value: string; note?: string }) {
  return (
    <div className="usage-stat">
      <span>{label}</span>
      <strong>{value}</strong>
      {note && <small>{note}</small>}
    </div>
  );
}

/** The figure shown next to Usage in the sidebar. */
export function todayLabel(cost: number | null): string | null {
  return cost == null || cost === 0 ? null : `Today ${formatUsd(cost)}`;
}
