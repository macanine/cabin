import { Progress } from "@mantine/core";
import { useEffect, useState } from "react";
import { formatDateTime } from "../lib/time";

/** The provider's current window, not an estimate from local token logs. */
export function AccountLimit({ label, used, resetsAt, resetText }: {
  label: string;
  used: number;
  resetsAt?: number | null;
  resetText?: string | null;
}) {
  const [now, setNow] = useState(Date.now);
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), 60_000);
    return () => clearInterval(timer);
  }, []);
  const remaining = resetsAt == null ? null : Math.max(0, resetsAt - now);
  const minutes = remaining == null ? null : Math.ceil(remaining / 60_000);
  const countdown = minutes == null ? null : minutes === 0 ? "reset due" : minutes < 60
    ? `in ${minutes}m` : `in ${Math.floor(minutes / 60)}h ${minutes % 60}m`;
  const percent = Math.max(0, Math.min(100, used));
  return (
    <div className="account-limit">
      <div className="account-limit-heading"><strong>{label}</strong><span><b>{percent}%</b> used</span></div>
      <Progress value={percent} color={percent >= 90 ? "red" : percent >= 70 ? "yellow" : "teal"} size="sm" radius="xl" aria-label={`${label}: ${percent}% used`} />
      <div className="account-limit-reset">{resetsAt != null ? `Resets ${formatDateTime(resetsAt)} · ${countdown}` : resetText ? `Resets ${resetText}` : "Reset time unavailable"}</div>
    </div>
  );
}
