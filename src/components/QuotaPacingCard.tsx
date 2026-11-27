import { Tooltip } from "@mantine/core";
import { AgentLogo, AGENT_NAMES } from "./common";
import { BurnChart, burnable } from "./BurnChart";
import { PaceMeter } from "./PaceMeter";
import {
  forecastLimit,
  formatSpan,
  type LimitForecast,
  type PaceStatus,
} from "../lib/agentLimits";
import { formatClockTime, formatDateTime } from "../lib/time";
import type { UnifiedQuotaWindow } from "../state/quotaStore";
import { useI18n } from "../i18n";

function pacePill(
  forecast: LimitForecast,
  t: ReturnType<typeof useI18n>["t"],
): { text: string; tone: "ok" | "warn" | "bad" | "muted" } {
  const status: PaceStatus = forecast.status;
  if (status === "exhausted") return { text: t("quota.pace.exhausted"), tone: "bad" };
  if (status === "fast" && forecast.paceRatio != null) {
    return { text: t("quota.pace.ratioFast", { ratio: forecast.paceRatio.toFixed(1) }), tone: "bad" };
  }
  if (status === "even" && forecast.paceRatio != null) {
    return {
      text: t("quota.pace.ratioEven", { ratio: forecast.paceRatio.toFixed(1) }),
      tone: forecast.tone === "warn" ? "warn" : "ok",
    };
  }
  if (status === "under" && forecast.paceRatio != null) {
    return { text: t("quota.pace.ratioUnder", { ratio: forecast.paceRatio.toFixed(1) }), tone: "ok" };
  }
  if (status === "early") {
    return { text: forecast.used === 0 ? t("quota.pace.idle") : t("quota.pace.early"), tone: "muted" };
  }
  return { text: t("quota.pace.live"), tone: "muted" };
}

function tierTag(tier: UnifiedQuotaWindow["tier"], t: ReturnType<typeof useI18n>["t"]): string {
  if (tier === "5h") return t("quota.tier.5h");
  if (tier === "7d") return t("quota.tier.7d");
  return t("quota.tier.other");
}

/**
 * Unified Magpie-inspired Quota & Pacing Card for one rolling plan window
 * (5h session window, 7d weekly window, or scoped model cap).
 */
export function QuotaPacingCard({
  window,
  now,
  showAgent = false,
  compact = false,
  onSelect,
}: {
  window: UnifiedQuotaWindow;
  now: number;
  /** Show provider logo & name (used in cross-agent Overview). */
  showAgent?: boolean;
  compact?: boolean;
  onSelect?: () => void;
}) {
  const { t } = useI18n();
  const forecast = forecastLimit(window, now);
  const left = Math.round(forecast.left);
  const used = Math.round(forecast.used);
  const pill = pacePill(forecast, t);
  const clockLabel = window.resetsAt != null ? formatClockTime(window.resetsAt, now) : window.resetText;
  const fullReset = window.resetsAt != null ? formatDateTime(window.resetsAt) : window.resetText;
  const chartHeight = compact ? 68 : 88;

  const content = (
    <>
      <div className="qp-head">
        <div className="qp-title-group">
          {showAgent && (
            <span className="qp-agent-chip">
              <AgentLogo agent={window.agent} size={14} />
              <span>{AGENT_NAMES[window.agent]}</span>
            </span>
          )}
          <span className="qp-tier-badge" data-tier={window.tier}>{tierTag(window.tier, t)}</span>
          <span className="qp-name">{window.shortLabel}</span>
        </div>
        {forecast.resetsIn != null ? (
          <Tooltip label={fullReset ? t("quota.resetsAtExact", { time: fullReset }) : undefined} disabled={!fullReset} openDelay={250}>
            <span className="qp-reset">
              <strong>{formatSpan(forecast.resetsIn)}</strong>
              {clockLabel && <span className="qp-reset-clock">· {clockLabel}</span>}
            </span>
          </Tooltip>
        ) : clockLabel ? (
          <span className="qp-reset">{clockLabel}</span>
        ) : null}
      </div>

      <div className="qp-hero">
        <div className="qp-figure">
          <strong>{left}<small>%</small></strong>
          <span className="qp-figure-label">{t("agents.left")}</span>
          <span className="qp-used-sub">{t("quota.usedPct", { pct: used })}</span>
        </div>
        <span className="qp-pace-pill" data-tone={pill.tone}>{pill.text}</span>
      </div>

      {burnable(window, now) ? (
        <BurnChart
          window={window}
          forecast={forecast}
          scope={window.scope}
          now={now}
          height={chartHeight}
          axis
        />
      ) : (
        <div className="qp-meter-wrap">
          <PaceMeter forecast={forecast} label={`${AGENT_NAMES[window.agent]} ${window.shortLabel}: ${left}%`} />
        </div>
      )}

      <div className="qp-foot">
        <span className="qp-forecast" data-tone={forecast.tone}>{forecast.text}</span>
        {forecast.sustainableRate && (
          <span className="qp-budget">
            {forecast.sustainableRate.unit === "h"
              ? t("quota.budgetHour", { rate: Math.round(forecast.sustainableRate.value) })
              : t("quota.budgetDay", { rate: Math.round(forecast.sustainableRate.value) })}
          </span>
        )}
      </div>
    </>
  );

  if (onSelect) {
    return (
      <div
        className="qp-card clickable press"
        data-tone={forecast.tone}
        role="button"
        tabIndex={0}
        onClick={onSelect}
        onKeyDown={(event) => {
          if (event.key === "Enter" || event.key === " ") {
            event.preventDefault();
            onSelect();
          }
        }}
      >
        {content}
      </div>
    );
  }

  return (
    <div className="qp-card" data-tone={forecast.tone} role="group" aria-label={`${AGENT_NAMES[window.agent]} · ${window.shortLabel}: ${left}% ${t("agents.left")}`}>
      {content}
    </div>
  );
}
