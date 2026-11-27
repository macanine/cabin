import { Tooltip } from "@mantine/core";
import {
  IconAlertTriangle, IconCheck, IconCopy, IconEye, IconEyeOff, IconMoon, IconSun,
} from "@tabler/icons-react";
import { useEffect, useState, type ReactNode } from "react";
import { clipboard } from "../../services/ipc/dialogs";
import { daylight, maskIp, NEXT_VISIBILITY, type IpVisibility } from "../../lib/geo";
import { Flag } from "../Flag";

const VISIBILITY_KEY = "cabin.ipVisibility";
const VISIBILITY_LABEL: Record<IpVisibility, string> = {
  hidden: "Hidden — show the ends",
  partial: "Partly hidden — show all",
  shown: "Shown — hide again",
};

function readVisibility(): IpVisibility {
  try {
    const value = localStorage.getItem(VISIBILITY_KEY);
    return value === "partial" || value === "shown" ? value : "hidden";
  } catch {
    return "hidden";
  }
}

/** An address in three privacy levels (cycled by the eye), copied whole. */
function Address({ ip, privacy }: { ip: string; privacy: boolean }) {
  const [visibility, setVisibility] = useState<IpVisibility>(privacy ? readVisibility : () => "shown");
  const [copied, setCopied] = useState(false);
  useEffect(() => {
    if (!copied) return;
    const timer = window.setTimeout(() => setCopied(false), 1400);
    return () => window.clearTimeout(timer);
  }, [copied]);

  function cycle() {
    const next = NEXT_VISIBILITY[visibility];
    setVisibility(next);
    try { localStorage.setItem(VISIBILITY_KEY, next); } catch { /* not remembered */ }
  }

  return (
    <span className="flex w-full min-w-0 items-center gap-1">
      <span className="net-ip-value mono" title={visibility === "shown" ? ip : undefined} aria-label={visibility === "shown" ? ip : "Address, hidden"}>
        {maskIp(ip, visibility).map((part, index) => (
          <span key={index} className={part.blur ? "net-blur" : undefined}>{part.text}</span>
        ))}
      </span>
      <span className="ml-auto flex shrink-0 items-center gap-0.5">
        {privacy && (
          <Tooltip label={VISIBILITY_LABEL[visibility]} openDelay={300}>
            <button type="button" className="net-icon-button" onClick={cycle} aria-label={VISIBILITY_LABEL[visibility]}>
              {visibility === "shown" ? <IconEye size={14} /> : <IconEyeOff size={14} />}
            </button>
          </Tooltip>
        )}
        <Tooltip label={copied ? "Copied" : "Copy the address"} openDelay={200}>
          <button type="button" className="net-icon-button" aria-label="Copy the address" onClick={() => void clipboard.write(ip).then(() => setCopied(true)).catch(() => undefined)}>
            {copied ? <IconCheck size={13} className="text-ok" /> : <IconCopy size={13} />}
          </button>
        </Tooltip>
      </span>
    </span>
  );
}

function clock(ms: number, timeZone: string | null): string {
  const options: Intl.DateTimeFormatOptions = { hour: "2-digit", minute: "2-digit" };
  try {
    return new Intl.DateTimeFormat(undefined, { ...options, timeZone: timeZone ?? undefined }).format(ms);
  } catch {
    return new Intl.DateTimeFormat(undefined, options).format(ms);
  }
}

/** Local time and light at a point on the route. */
function Daylight({ lat, lon, timeZone }: { lat: number; lon: number; timeZone: string | null }) {
  const [now, setNow] = useState(Date.now);
  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), 30_000);
    return () => window.clearInterval(timer);
  }, []);
  const light = daylight(lat, lon, now);
  return (
    <span className="flex min-w-0 items-center gap-1.5" title={timeZone ? `Local time (${timeZone})` : "Local time"}>
      {light.day ? <IconSun size={14} className="shrink-0 text-warn" /> : <IconMoon size={14} className="shrink-0 text-accent" />}
      <strong className="mono font-semibold tabular-nums">{clock(now, timeZone)}</strong>
      <span className="truncate text-muted">{light.polar ?? light.phase.split(",")[0]}</span>
    </span>
  );
}

export interface PointInfo {
  flag: string | null;
  title: string;
  sub: string;
  ip: string | null;
  /** Mac and exit addresses are private; node servers are not. */
  privacy: boolean;
  isp: string | null;
  asn: number | null;
  place: { lat: number; lon: number; timeZone: string | null } | null;
  loading: boolean;
  error: string | null;
  classification?: { label: string; detail: string | null };
}

/** One labelled fact; label sits above the value so addresses and networks never crush. */
function Fact({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="net-point-fact">
      <dt>{label}</dt>
      <dd>{children}</dd>
    </div>
  );
}

/**
 * One point of the route (this computer, a node, the exit): full-width
 * header with location, IP type and optional view switcher, a clean inline
 * alert when refresh fails, then a responsive multi-column metric strip.
 */
export function PointCard({ info, compact = false, actions }: {
  info: PointInfo;
  compact?: boolean;
  actions?: ReactNode;
}) {
  const failed = Boolean(info.error && !info.ip);
  const hasFacts = Boolean(info.ip || info.isp || info.place);
  return (
    <div className={`net-point${compact ? " compact" : ""}${failed ? " failed" : ""}`}>
      <div className="net-point-head">
        <div className="net-point-id">
          <span className="net-flag" aria-hidden><Flag code={info.flag} size={compact ? 14 : 17} pending={info.loading} /></span>
          {info.loading && <span className="sr-only" role="status">Refreshing location</span>}
          <div className="min-w-0 flex-1">
            <div className="net-point-heading">
              <strong className="net-point-title" title={info.title}>{info.title}</strong>
              {info.classification && (
                <Tooltip label={info.classification.detail ? `Source: ${info.classification.detail}` : "IP type"} openDelay={300} disabled={!info.classification.detail}>
                  <span className="net-point-badge">{info.classification.label}</span>
                </Tooltip>
              )}
            </div>
            <div className="net-point-sub" title={info.sub}>{info.sub}</div>
          </div>
        </div>
        {actions && <div className="net-point-actions">{actions}</div>}
      </div>
      {info.error && !info.loading && (
        <div className="net-exit-error" role="alert">
          <IconAlertTriangle size={14} />
          <span>{info.error}</span>
        </div>
      )}
      {hasFacts && (
        <dl className="net-point-facts">
          {info.ip ? (
            <Fact label="Address"><Address key={info.ip} ip={info.ip} privacy={info.privacy} /></Fact>
          ) : null}
          {info.isp && (
            <Fact label="Network">
              <span className="net-point-network">
                <span className="net-point-isp" title={info.isp}>{info.isp}</span>
                {info.asn != null && <span className="net-point-asn mono">AS{info.asn}</span>}
              </span>
            </Fact>
          )}
          {info.place && (
            <Fact label="Local time"><Daylight lat={info.place.lat} lon={info.place.lon} timeZone={info.place.timeZone} /></Fact>
          )}
        </dl>
      )}
    </div>
  );
}
