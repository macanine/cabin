import { Button, Loader, SegmentedControl, Select, Tooltip } from "@mantine/core";
import { IconActivity, IconBolt, IconCoin, IconStack2, IconX } from "@tabler/icons-react";
import { useEffect, useMemo, useState, type ReactNode } from "react";
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
const METRIC_HEADING: Record<UsageMetric, string> = {
  cost: "Daily spend",
  tokens: "Daily tokens",
  requests: "Daily requests",
};
type Tab = "requests" | "models" | "agents" | "pricing";
const TABS: { value: Tab; label: string }[] = [
  { value: "requests", label: "Requests" },
  { value: "models", label: "Models" },
  { value: "agents", label: "Agents" },
  { value: "pricing", label: "Pricing" },
];

const RANGE_SHORT: Record<UsageRange, string> = { "1": "1D", "7": "7D", "30": "30D", "90": "90D", all: "All" };
const RANGE_NAMES: Record<UsageRange, string> = { "1": "Today", "7": "Last 7 days", "30": "Last 30 days", "90": "Last 90 days", all: "All time" };

function readRange(): UsageRange {
  try {
    const saved = localStorage.getItem(RANGE_KEY);
    return USAGE_RANGES.some((range) => range.value === saved) ? (saved as UsageRange) : "30";
  } catch {
    return "30";
  }
}

const agentName = (agent: string) => (isAgentKind(agent) ? AGENT_NAMES[agent] : agent);

/**
 * Usage analytics dashboard: top filter bar with 1-click period & agent pills,
 * 4 KPI summary cards, full-width interactive trend chart, balanced 2-column
 * agent & model breakdowns, and tabbed request/model/pricing explorer.
 */
export function UsageView() {
  const usage = useUsage();
  const [pricesOpen, setPricesOpen] = useState(false);
  const [dashboard, setDashboard] = useState<UsageDashboard | null>(null);
  return (
    <>
      <ViewHeader title="Usage" subtitle={usage.syncedAt != null ? `Synced ${formatTime(usage.syncedAt)}` : "Syncing agent records…"}>
        <Button size="xs" variant="default" leftSection={<IconCoin size={14} />} onClick={() => setPricesOpen(true)}>
          Model prices
        </Button>
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
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [fromMs, agent, model, page, usage.revision, zone, clockOffset, onDashboard]);

  const summary = dashboard?.summary;
  const hitRate = summary ? cacheHitRate(summary) : null;
  const options = [...new Set([...modelOptions, ...(model ? [model] : [])])].sort();
  const pageCount = Math.max(1, Math.ceil((dashboard?.total ?? 0) / PAGE_SIZE));
  const shares = useMemo(() => (dashboard ? modelShares(dashboard.models, metric, 50) : []), [dashboard, metric]);
  const agents = useMemo(() => (dashboard?.agents ?? []).slice().sort((a, b) => b.cost - a.cost), [dashboard]);
  const modelsSorted = useMemo(() => (dashboard?.models ?? []).slice().sort((a, b) => b.cost - a.cost), [dashboard]);

  const totalTok = summary ? totalTokens(summary) : 0;
  const inPct = totalTok > 0 && summary ? (summary.input / totalTok) * 100 : 0;
  const outPct = totalTok > 0 && summary ? (summary.output / totalTok) * 100 : 0;
  const cachePct = totalTok > 0 && summary ? ((summary.cacheRead + summary.cacheWrite) / totalTok) * 100 : 0;
  const activeDays = dashboard?.daily.length || 1;
  const pageStart = dashboard?.total ? page * PAGE_SIZE + 1 : 0;
  const pageEnd = dashboard ? Math.min(dashboard.total, (page + 1) * PAGE_SIZE) : 0;

  return (
    <div className="usg-page">
      <div className="usg-inner">
        <div className="usg-toolbar">
          <AgentChips value={agent} onChange={(next) => { setAgent(next); setModel(null); setPage(0); }} />
          <div className="usg-toolbar-right">
            {options.length > 1 && (
              <Select
                aria-label="Filter by model"
                placeholder="All models"
                size="xs"
                clearable
                searchable
                value={model}
                data={options}
                onChange={(value) => { setModel(value); setPage(0); }}
                className="usg-model-select"
              />
            )}
            <div className="usg-range-pills" role="radiogroup" aria-label="Time period">
              {USAGE_RANGES.map((entry) => {
                const active = range === entry.value;
                return (
                  <Tooltip key={entry.value} label={RANGE_NAMES[entry.value]} openDelay={350}>
                    <button
                      type="button"
                      role="radio"
                      aria-checked={active}
                      className="press"
                      onClick={() => setRange(entry.value)}
                    >
                      {RANGE_SHORT[entry.value]}
                    </button>
                  </Tooltip>
                );
              })}
            </div>
          </div>
        </div>

        {(agent || model) && (
          <div className="usg-active-filters">
            <span className="text-xs text-faint">Filtered by</span>
            {agent && (
              <button type="button" className="usg-filter-chip press" onClick={() => { setAgent(null); setModel(null); setPage(0); }}>
                <span>{agentName(agent)}</span>
                <IconX size={12} />
              </button>
            )}
            {model && (
              <button type="button" className="usg-filter-chip mono press" onClick={() => { setModel(null); setPage(0); }}>
                <span className="truncate max-w-[240px]">{model}</span>
                <IconX size={12} />
              </button>
            )}
            <button type="button" className="usg-clear-link" onClick={() => { setAgent(null); setModel(null); setPage(0); }}>
              Clear all
            </button>
          </div>
        )}

        <UsageAlerts error={usage.error ?? error} importErrors={usage.importErrors}
          onCloseError={() => { usage.clearError(); setError(null); }} onCloseImport={usage.clearImportErrors} />

        {!dashboard || !summary ? (
          <div className="usg-loading">{loading ? <><Loader size="sm" /> Loading usage…</> : "No usage data."}</div>
        ) : (
          <div className={`usg-results${loading ? " loading" : ""}`} aria-busy={loading}>
            <div className="usg-kpis stagger">
              <KpiCard
                tone="accent"
                icon={<IconCoin size={16} stroke={1.9} />}
                label={`Spend · ${RANGE_NAMES[range]}`}
                value={formatUsd(summary.cost)}
                foot={
                  summary.unpricedRequests > 0
                    ? `+ ${formatInteger(summary.unpricedRequests)} unpriced`
                    : `${formatUsd(summary.cost / activeDays)} / active day`
                }
              />
              <KpiCard
                tone="info"
                icon={<IconActivity size={16} stroke={1.9} />}
                label="Requests"
                value={formatInteger(summary.requests)}
                foot={`${formatInteger(dashboard.models.length)} ${dashboard.models.length === 1 ? "model" : "models"} · ${formatInteger(dashboard.agents.length)} ${dashboard.agents.length === 1 ? "agent" : "agents"}`}
              />
              <KpiCard
                tone="ok"
                icon={<IconStack2 size={16} stroke={1.9} />}
                label="Total tokens"
                value={formatCount(totalTok)}
                bar={
                  totalTok > 0 ? (
                    <span className="usg-tok-bar" title={`Input ${formatCount(summary.input)} · Output ${formatCount(summary.output)} · Cache ${formatCount(summary.cacheRead + summary.cacheWrite)}`}>
                      <span className="in" style={{ width: `${inPct}%` }} />
                      <span className="out" style={{ width: `${outPct}%` }} />
                      <span className="cache" style={{ width: `${cachePct}%` }} />
                    </span>
                  ) : null
                }
                foot={`${formatCount(summary.input)} in · ${formatCount(summary.output)} out`}
              />
              <KpiCard
                tone="warn"
                icon={<IconBolt size={16} stroke={1.9} />}
                label="Cache hit rate"
                value={hitRate === null ? "—" : `${(hitRate * 100).toFixed(1)}%`}
                bar={
                  hitRate !== null ? (
                    <span className="usg-hit-bar">
                      <span style={{ width: `${Math.min(100, hitRate * 100)}%` }} />
                    </span>
                  ) : null
                }
                foot={`${formatCount(summary.cacheRead)} read from cache`}
              />
            </div>

            <section className="usg-card usg-trend" aria-label="Activity trend">
              <div className="usg-card-head">
                <div>
                  <h2>{METRIC_HEADING[metric]}</h2>
                  <p>{RANGE_NAMES[range]}{agent ? ` · ${agentName(agent)}` : ""}{model ? ` · ${model}` : ""}</p>
                </div>
                <SegmentedControl size="xs" value={metric} data={METRICS} onChange={(value) => setMetric(value as UsageMetric)} aria-label="Chart metric" />
              </div>
              <DailyBars dashboard={dashboard} metric={metric} range={range} />
            </section>

            <div className="usg-split">
              <BreakdownCard
                title="By agent"
                empty="No agent activity in this period."
                rows={agents.map((row) => ({
                  key: row.agent,
                  label: agentName(row.agent),
                  meta: `${formatInteger(row.requests)} req`,
                  cost: row.cost,
                }))}
                active={agent}
                onPick={(key) => { if (isAgentKind(key)) { setAgent(agent === key ? null : key); setModel(null); setPage(0); } }}
              />
              <BreakdownCard
                title="Top models"
                mono
                empty="No model activity in this period."
                rows={modelsSorted.map((row) => ({
                  key: row.model,
                  label: row.model,
                  meta: `${formatInteger(row.requests)} req`,
                  cost: row.cost,
                }))}
                active={model}
                onPick={(key) => { setModel(model === key ? null : key); setPage(0); }}
              />
            </div>

            <section className="usg-card usg-explorer">
              <div className="usg-explorer-head">
                <div className="usg-tabs" role="tablist" aria-label="Details">
                  {TABS.map((entry) => (
                    <button key={entry.value} type="button" role="tab" aria-selected={tab === entry.value} className="press" onClick={() => setTab(entry.value)}>
                      {entry.label}
                      {entry.value === "requests" && <span className="usg-tab-count">{formatCount(dashboard.total)}</span>}
                      {entry.value === "models" && <span className="usg-tab-count">{dashboard.models.length}</span>}
                    </button>
                  ))}
                </div>
                {tab === "requests" && dashboard.total > 0 && (
                  <span className="usg-range-meta tabular-nums">Showing {pageStart}–{pageEnd} of {formatInteger(dashboard.total)}</span>
                )}
              </div>

              {tab === "requests" && (dashboard.logs.length ? (
                <>
                  <div className="usg-table-wrap">
                    <table className="usg-table">
                      <thead>
                        <tr><th>Time</th><th>Agent</th><th>Model</th><th className="num">Input</th><th className="num">Output</th><th className="num">Cache read</th><th className="num">Cost</th></tr>
                      </thead>
                      <tbody>
                        {dashboard.logs.map((log) => (
                          <tr key={log.id} tabIndex={0} onClick={() => setSelectedLog(log)} onKeyDown={(event) => event.key === "Enter" && setSelectedLog(log)}>
                            <td className="muted-cell">{formatGuestDateTime(log.timestamp)}</td>
                            <td><span className="usg-agent-tag">{agentName(log.agent)}</span></td>
                            <td className="mono usg-model-cell" title={log.model}>{log.model}</td>
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
                    <div className="usg-pager">
                      <Button size="compact-xs" variant="default" disabled={loading || page === 0} onClick={() => setPage(page - 1)}>Newer</Button>
                      <span>Page {page + 1} of {pageCount}</span>
                      <Button size="compact-xs" variant="default" disabled={loading || page + 1 >= pageCount} onClick={() => setPage(page + 1)}>Older</Button>
                    </div>
                  )}
                </>
              ) : <div className="usg-empty">No requests in this period.</div>)}

              {tab === "models" && (shares.length ? (
                <div className="usg-table-wrap">
                  <table className="usg-table">
                    <thead><tr><th>Model</th><th className="num">Requests</th><th className="num">Tokens</th><th className="num">Cost</th><th className="usg-share-col">Share of {metric}</th></tr></thead>
                    <tbody>
                      {shares.map((row) => {
                        const item = dashboard.models.find((entry) => entry.model === row.key);
                        return (
                          <tr key={row.key} tabIndex={0} onClick={() => { setModel(model === row.key ? null : row.key); setPage(0); }} onKeyDown={(event) => event.key === "Enter" && (setModel(model === row.key ? null : row.key), setPage(0))}>
                            <td className="mono usg-model-cell" title={row.key}>{row.key}</td>
                            <td className="num">{formatInteger(row.requests)}</td>
                            <td className="num">{item ? formatCount(totalTokens(item)) : "—"}</td>
                            <td className="num strong">{item ? formatUsd(item.cost) : formatMetric(row.value, metric)}</td>
                            <td className="usg-share-col">
                              <span className="usg-share-track"><span style={{ width: `${row.share * 100}%` }} /></span>
                              <small>{Math.round(row.percent * 100)}%</small>
                            </td>
                          </tr>
                        );
                      })}
                    </tbody>
                  </table>
                </div>
              ) : <div className="usg-empty">No models in this period.</div>)}

              {tab === "agents" && (agents.length ? (
                <div className="usg-table-wrap">
                  <table className="usg-table">
                    <thead><tr><th>Agent</th><th className="num">Requests</th><th className="num">Tokens</th><th className="num">Cache hit</th><th className="num">Cost</th></tr></thead>
                    <tbody>
                      {agents.map((row) => {
                        const rate = cacheHitRate(row);
                        return (
                          <tr key={row.agent} tabIndex={0} onClick={() => isAgentKind(row.agent) && (setAgent(agent === row.agent ? null : row.agent), setPage(0))}>
                            <td><span className="usg-agent-tag">{agentName(row.agent)}</span></td>
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
              ) : <div className="usg-empty">No agent activity in this period.</div>)}

              {tab === "pricing" && (
                <div className="usg-pricing">
                  <div className="usg-pricing-banner">
                    <p className="m-0 text-xs text-muted leading-relaxed">
                      Costs use agent-reported amounts when available, falling back to API-equivalent estimates from models.dev
                      {dashboard.priceSources[0]?.lastSuccess != null ? ` (${formatInteger(dashboard.priceSources[0].count)} models, updated ${formatGuestDateTime(dashboard.priceSources[0].lastSuccess)})` : ""}.
                      Your custom overrides always take precedence.
                    </p>
                    <Button size="xs" variant="default" leftSection={<IconCoin size={14} />} onClick={onEditPrices}>Edit prices</Button>
                  </div>
                  {dashboard.prices.length ? (
                    <div className="usg-table-wrap">
                      <table className="usg-table">
                        <thead><tr><th>Model</th><th className="num">Input</th><th className="num">Output</th><th className="num">Cache read</th><th className="num">Cache write</th></tr></thead>
                        <tbody>
                          {dashboard.prices.slice().sort((a, b) => a.model.localeCompare(b.model)).map((price) => (
                            <tr key={price.model}>
                              <td className="mono usg-model-cell">{price.model}</td>
                              <td className="num">{formatPrice(price.input)}</td>
                              <td className="num">{formatPrice(price.output)}</td>
                              <td className="num">{formatPrice(price.cacheRead)}</td>
                              <td className="num">{formatPrice(price.cacheWrite)}</td>
                            </tr>
                          ))}
                        </tbody>
                      </table>
                    </div>
                  ) : <div className="usg-empty">No custom price overrides yet. Prices are per 1M tokens.</div>}
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

function KpiCard({ tone, icon, label, value, bar, foot }: {
  tone: "accent" | "info" | "ok" | "warn";
  icon: ReactNode;
  label: string;
  value: string;
  bar?: ReactNode;
  foot: string;
}) {
  return (
    <section className="usg-kpi" data-tone={tone}>
      <div className="usg-kpi-top">
        <span className="usg-kpi-label">{label}</span>
        <span className="usg-kpi-icon" aria-hidden>{icon}</span>
      </div>
      <strong className="usg-kpi-value">{value}</strong>
      {bar}
      <span className="usg-kpi-foot">{foot}</span>
    </section>
  );
}

/** Balanced breakdown card: generous width, clear percentage + bar, click to filter. */
function BreakdownCard({ title, empty, rows, active, onPick, mono = false }: {
  title: string;
  empty: string;
  rows: { key: string; label: string; meta: string; cost: number }[];
  active: string | null;
  onPick: (key: string) => void;
  mono?: boolean;
}) {
  const total = rows.reduce((sum, row) => sum + row.cost, 0);
  const shown = rows.slice(0, 6);
  return (
    <section className="usg-card usg-breakdown" aria-label={title}>
      <div className="usg-breakdown-head">
        <h2>{title}</h2>
        {rows.length > 0 && <span>{rows.length} total</span>}
      </div>
      {shown.length === 0 ? (
        <p className="m-0 py-6 text-center text-xs text-faint">{empty}</p>
      ) : (
        <div className="usg-breakdown-list">
          {shown.map((row) => {
            const share = total > 0 ? (row.cost / total) * 100 : 0;
            return (
              <button
                key={row.key}
                type="button"
                className="usg-share press"
                aria-pressed={active === row.key}
                title={row.label}
                onClick={() => onPick(row.key)}
              >
                <div className="usg-share-row">
                  <span className={`usg-share-name ${mono ? "mono" : ""}`}>{row.label}</span>
                  <span className="usg-share-meta">{row.meta} · {Math.round(share)}%</span>
                  <strong className="usg-share-cost">{formatUsd(row.cost)}</strong>
                </div>
                <span className="usg-share-bar" aria-hidden>
                  <span style={{ inlineSize: `${Math.max(share > 0 ? 2 : 0, share)}%` }} />
                </span>
              </button>
            );
          })}
        </div>
      )}
    </section>
  );
}

/** The figure shown next to Usage in the sidebar. */
export function todayLabel(cost: number | null): string | null {
  return cost == null || cost === 0 ? null : `Today ${formatUsd(cost)}`;
}
