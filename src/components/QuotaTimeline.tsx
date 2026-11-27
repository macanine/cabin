import { Tooltip } from "@mantine/core";
import { IconClock, IconCreditCard, IconRotateClockwise, IconSparkles } from "@tabler/icons-react";
import { AgentLogo, AGENT_NAMES } from "./common";
import {
  buildResetTimeline,
  FIVE_HOUR_MS,
  forecastLimit,
  formatSpan,
  SEVEN_DAY_MS,
  type TimelineCluster,
} from "../lib/agentLimits";
import { formatClockTime, formatDate, formatDateTime } from "../lib/time";
import type { AgentKind } from "../services/ipc/generated/core";
import type { AgentQuotaSummary, UnifiedQuotaWindow } from "../state/quotaStore";
import { useI18n } from "../i18n";

const AGENT_COLOR: Record<AgentKind, string> = {
  claude: "#d97757",
  codex: "#10a37f",
  antigravity: "#4285f4",
  opencode: "#6366f1",
  pi: "#ec4899",
};

function DonutItem({
  window,
  now,
  onSelect,
}: {
  window: UnifiedQuotaWindow;
  now: number;
  onSelect: (agent: AgentKind) => void;
}) {
  const { t } = useI18n();
  const forecast = forecastLimit(window, now);
  const left = Math.round(forecast.left);
  const r = 13;
  const circ = 2 * Math.PI * r;
  const dash = (left / 100) * circ;
  const color = forecast.tone === "bad" ? "var(--bad)" : forecast.tone === "warn" ? "var(--warn)" : AGENT_COLOR[window.agent];
  const resetClock = window.resetsAt != null ? formatClockTime(window.resetsAt, now) : "";
  const tip = [
    `${AGENT_NAMES[window.agent]} · ${window.shortLabel}`,
    `${left}% ${t("agents.left")} (${Math.round(forecast.used)}% ${t("quota.usedShort")})`,
    forecast.text,
    window.resetsAt != null ? `${t("quota.resetsAtExact", { time: formatDateTime(window.resetsAt) })}` : null,
  ].filter(Boolean).join(" · ");

  return (
    <Tooltip label={tip} multiline maw={280} openDelay={160}>
      <button
        type="button"
        className="qt-donut press"
        onClick={() => onSelect(window.agent)}
        aria-label={tip}
      >
        <svg width="34" height="34" viewBox="0 0 34 34" aria-hidden>
          <circle cx="17" cy="17" r={r} fill="none" stroke="color-mix(in srgb, var(--text) 10%, transparent)" strokeWidth="3" />
          <circle
            cx="17"
            cy="17"
            r={r}
            fill="none"
            stroke={color}
            strokeWidth="3"
            strokeLinecap="round"
            strokeDasharray={`${dash.toFixed(1)} ${circ.toFixed(1)}`}
            transform="rotate(-90 17 17)"
          />
        </svg>
        <span className="qt-donut-logo"><AgentLogo agent={window.agent} size={13} /></span>
        <span className="qt-donut-pct">{left}%</span>
        {resetClock && <span className="qt-donut-time">{resetClock}</span>}
      </button>
    </Tooltip>
  );
}

function TimelineBar({
  window,
  now,
  onSelect,
}: {
  window: UnifiedQuotaWindow;
  now: number;
  onSelect: (agent: AgentKind) => void;
}) {
  const { t } = useI18n();
  const forecast = forecastLimit(window, now);
  const left = Math.round(forecast.left);
  const color = forecast.tone === "bad" ? "var(--bad)" : forecast.tone === "warn" ? "var(--warn)" : AGENT_COLOR[window.agent];
  const resetClock = window.resetsAt != null ? formatClockTime(window.resetsAt, now) : "";
  const remainText = forecast.resetsIn != null ? formatSpan(forecast.resetsIn) : "";
  const tip = [
    `${AGENT_NAMES[window.agent]}${window.plan ? ` ${window.plan}` : ""} · ${window.shortLabel}`,
    `${left}% ${t("agents.left")} · ${forecast.text}`,
    window.resetsAt != null ? t("quota.resetsAtExact", { time: formatDateTime(window.resetsAt) }) : null,
  ].filter(Boolean).join("\n");

  // Remaining-quota bar with an even-pace ideal-remaining tick (1 - elapsed).
  const idealLeftPct = forecast.elapsed != null ? Math.max(0, Math.min(100, (1 - forecast.elapsed) * 100)) : null;

  return (
    <Tooltip label={tip} multiline maw={280} openDelay={160}>
      <button
        type="button"
        className="qt-bar press"
        data-tone={forecast.tone}
        onClick={() => onSelect(window.agent)}
      >
        <span className="qt-bar-brand" style={{ ["--agent-color" as string]: color }}>
          <AgentLogo agent={window.agent} size={14} />
          <span className="qt-bar-label">{window.shortLabel}</span>
        </span>
        <span className="qt-bar-track" aria-hidden>
          <span className="qt-bar-fill" style={{ inlineSize: `${Math.max(left > 0 ? 3 : 0, left)}%`, background: color }} />
          {idealLeftPct != null && (
            <span className="qt-bar-tick" style={{ insetInlineStart: `${idealLeftPct}%` }} />
          )}
        </span>
        <span className="qt-bar-right">
          <strong className="qt-bar-pct">{left}%</strong>
          <span className="qt-bar-when">{remainText}{resetClock ? ` · ${resetClock}` : ""}</span>
        </span>
      </button>
    </Tooltip>
  );
}

function HorizonCard({
  title,
  horizonLabel,
  ticks,
  clusters,
  now,
  emptyText,
  onSelectAgent,
  footer,
}: {
  title: string;
  horizonLabel: string;
  ticks: { pos: number; label: string }[];
  clusters: TimelineCluster<UnifiedQuotaWindow>[];
  now: number;
  emptyText: string;
  onSelectAgent: (agent: AgentKind) => void;
  footer?: React.ReactNode;
}) {
  const { t } = useI18n();
  const totalCount = clusters.reduce((sum, cluster) => sum + cluster.entries.length, 0);
  const soonest = clusters[0]?.resetsAt ?? null;

  return (
    <section className="qt-card">
      <div className="qt-card-head">
        <div className="qt-card-title">
          <h3>{title}</h3>
          <span className="qt-horizon-pill">{horizonLabel}</span>
        </div>
        {soonest != null ? (
          <span className="qt-soonest">
            <IconClock size={12} aria-hidden />
            {t("quota.nextResetIn", { span: formatSpan(Math.max(0, soonest - now)) })}
          </span>
        ) : (
          <span className="qt-soonest muted">{totalCount === 0 ? t("quota.noneActive") : ""}</span>
        )}
      </div>

      {totalCount === 0 ? (
        <div className="qt-empty">{emptyText}</div>
      ) : (
        <div className="qt-canvas">
          <div className="qt-axis" aria-hidden>
            {ticks.map((tick) => (
              <div key={tick.label} className="qt-tick" style={{ bottom: `${tick.pos * 100}%` }}>
                <span>{tick.label}</span>
              </div>
            ))}
          </div>

          <div className="qt-plot">
            {ticks.map((tick) => (
              <div
                key={tick.label}
                className={`qt-gridline${tick.pos === 0 ? " now" : ""}`}
                style={{ bottom: `${tick.pos * 100}%` }}
                aria-hidden
              />
            ))}

            {clusters.map((cluster) => {
              const crowded = cluster.entries.length > 2;
              return (
                <div
                  key={`${cluster.resetsAt}:${cluster.entries.map((e) => e.item.id).join(",")}`}
                  className={`qt-cluster${crowded ? " crowded" : ""}`}
                  style={{ bottom: `${Math.max(0.06, Math.min(0.9, cluster.position)) * 100}%` }}
                >
                  {crowded
                    ? cluster.entries.map((entry) => (
                        <DonutItem key={entry.item.id} window={entry.item} now={now} onSelect={onSelectAgent} />
                      ))
                    : cluster.entries.map((entry) => (
                        <TimelineBar key={entry.item.id} window={entry.item} now={now} onSelect={onSelectAgent} />
                      ))}
                </div>
              );
            })}
          </div>
        </div>
      )}

      {footer}
    </section>
  );
}

/**
 * Magpie-style dual horizon reset timeline ("Next 5 hours" & "Next 7 days")
 * showing every active agent quota window anchored against "Now" at the bottom,
 * plus subscription expiry / credits / reset cards in one glanceable strip.
 */
export function QuotaTimeline({
  windows,
  byAgent,
  now,
  onSelectAgent,
}: {
  windows: readonly UnifiedQuotaWindow[];
  byAgent: Record<AgentKind, AgentQuotaSummary>;
  now: number;
  onSelectAgent: (agent: AgentKind) => void;
}) {
  const { t } = useI18n();

  const fiveHourWindows = windows.filter((w) => w.tier === "5h" || (w.resetsAt != null && w.resetsAt - now <= FIVE_HOUR_MS));
  const sevenDayWindows = windows.filter(
    (w) => w.tier === "7d" || (w.resetsAt != null && w.resetsAt - now > FIVE_HOUR_MS && w.resetsAt - now <= SEVEN_DAY_MS),
  );

  const clusters5h = buildResetTimeline(fiveHourWindows, FIVE_HOUR_MS, now, { clusterWindowMs: 12 * 60_000 });
  const clusters7d = buildResetTimeline(sevenDayWindows, SEVEN_DAY_MS, now, { minRemainingMs: FIVE_HOUR_MS, clusterWindowMs: 4 * 3_600_000 });

  // Collect subscription expiries, credits, and reset cards across agents.
  const codex = byAgent.codex;
  const extras: { key: string; icon: React.ReactNode; label: string; value: string; agent: AgentKind }[] = [];
  if (codex.subscriptionUntil != null) {
    extras.push({
      key: "codex-sub",
      icon: <IconSparkles size={13} />,
      label: t("quota.extra.subscription", { agent: AGENT_NAMES.codex }),
      value: t("quota.extra.paidThrough", { date: formatDate(codex.subscriptionUntil) }),
      agent: "codex",
    });
  }
  if (codex.credits && (codex.credits.unlimited || codex.credits.hasCredits)) {
    extras.push({
      key: "codex-credits",
      icon: <IconCreditCard size={13} />,
      label: t("quota.extra.credits", { agent: AGENT_NAMES.codex }),
      value: codex.credits.unlimited
        ? t("quota.extra.unlimited")
        : (codex.credits.balance ?? t("quota.extra.available")),
      agent: "codex",
    });
  }
  if (codex.resetCreditCount > 0) {
    const earliest = codex.resetCards.find((card) => card.expiresAt != null)?.expiresAt ?? null;
    extras.push({
      key: "codex-reset",
      icon: <IconRotateClockwise size={13} />,
      label: t("quota.extra.resetCards", { count: codex.resetCreditCount }),
      value: earliest != null
        ? t("quota.extra.expiresOn", { date: formatDate(earliest) })
        : t("quota.extra.available"),
      agent: "codex",
    });
  }

  return (
    <div className="qt-grid">
      <HorizonCard
        title={t("quota.next5h")}
        horizonLabel="5h"
        ticks={[
          { pos: 0, label: t("quota.now") },
          { pos: 0.2, label: "+1h" },
          { pos: 0.4, label: "+2h" },
          { pos: 0.6, label: "+3h" },
          { pos: 0.8, label: "+4h" },
          { pos: 1, label: "+5h" },
        ]}
        clusters={clusters5h}
        now={now}
        emptyText={t("quota.empty5h")}
        onSelectAgent={onSelectAgent}
      />

      <HorizonCard
        title={t("quota.next7d")}
        horizonLabel="7d"
        ticks={[
          { pos: 0, label: t("quota.now") },
          { pos: 1 / 7, label: "+1d" },
          { pos: 3 / 7, label: "+3d" },
          { pos: 5 / 7, label: "+5d" },
          { pos: 1, label: "+7d" },
        ]}
        clusters={clusters7d}
        now={now}
        emptyText={t("quota.empty7d")}
        onSelectAgent={onSelectAgent}
        footer={
          extras.length > 0 ? (
            <div className="qt-extras">
              {extras.map((item) => (
                <button
                  key={item.key}
                  type="button"
                  className="qt-extra-chip press"
                  onClick={() => onSelectAgent(item.agent)}
                >
                  <span className="qt-extra-icon" aria-hidden>{item.icon}</span>
                  <span className="qt-extra-label">{item.label}</span>
                  <strong>{item.value}</strong>
                </button>
              ))}
            </div>
          ) : null
        }
      />
    </div>
  );
}
