import { forecastLimit, shortLimitLabel } from "../lib/agentLimits";
import { formatDateTime } from "../lib/time";
import { useMinute } from "../lib/useNow";
import { PaceMeter } from "./PaceMeter";
import { BurnChart, burnable } from "./BurnChart";

/** The provider's current window with a pace forecast, not an estimate from local token logs. */
export function AccountLimit({ scope, label, used, resetsAt, resetText, windowMinutes }: {
  /** Agent and account the window belongs to. */
  scope: string;
  label: string;
  used: number;
  resetsAt?: number | null;
  resetText?: string | null;
  windowMinutes?: number | null;
}) {
  const now = useMinute();
  const window = { label, usedPercent: used, resetsAt: resetsAt ?? null, windowMinutes };
  const forecast = forecastLimit(window, now);
  const percent = Math.round(forecast.used);
  return (
    <div className="account-limit" data-tone={forecast.tone}>
      <div className="account-limit-heading"><strong>{shortLimitLabel(label)}</strong><span><b>{percent}%</b> used</span></div>
      {burnable(window, now)
        ? <BurnChart window={window} forecast={forecast} scope={scope} now={now} height={84} axis />
        : <PaceMeter forecast={forecast} label={`${label}: ${percent}% used`} />}
      <div className="account-limit-forecast">{forecast.text}</div>
      <div className="account-limit-reset">
        {resetsAt != null ? `Resets ${formatDateTime(resetsAt)}` : resetText ? `Resets ${resetText}` : "Reset time unavailable"}
      </div>
    </div>
  );
}
