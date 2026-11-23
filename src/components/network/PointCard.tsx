import { Loader, Tooltip } from "@mantine/core";
import {
  IconAlertTriangle, IconCheck, IconCopy, IconEye, IconEyeOff, IconMoon, IconSun,
} from "@tabler/icons-react";
import { useEffect, useState } from "react";
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
    <span className="flex min-w-0 items-center gap-1">
      <span className="net-ip-value mono" aria-label={visibility === "shown" ? ip : "Address, hidden"}>
        {maskIp(ip, visibility).map((part, index) => (
          <span key={index} className={part.blur ? "net-blur" : undefined}>{part.text}</span>
        ))}
      </span>
      {privacy && (
        <Tooltip label={VISIBILITY_LABEL[visibility]} openDelay={300}>
          <button className="net-icon-button" onClick={cycle} aria-label={VISIBILITY_LABEL[visibility]}>
            {visibility === "shown" ? <IconEye size={14} /> : <IconEyeOff size={14} />}
          </button>
        </Tooltip>
      )}
      <Tooltip label={copied ? "Copied" : "Copy the address"} openDelay={200}>
        <button className="net-icon-button" aria-label="Copy the address" onClick={() => void clipboard.write(ip).then(() => setCopied(true)).catch(() => undefined)}>
          {copied ? <IconCheck size={13} /> : <IconCopy size={13} />}
        </button>
      </Tooltip>
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

/** One labelled fact; every fact shares the label column so values align. */
function Fact({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="net-point-fact">
      <dt>{label}</dt>
      <dd>{children}</dd>
    </div>
  );
}

/**
 * One point of the route (this Mac, a node, the exit): the place and its IP
 * type on top, then aligned facts (address, network and AS, local time).
 */
export function PointCard({ info }: { info: PointInfo }) {
  const failed = Boolean(info.error && !info.ip);
  return (
    <div className={`net-point${failed ? " failed" : ""}`}>
      <div className="flex min-w-0 items-center gap-3">
        <span className="net-flag" aria-hidden><Flag code={info.flag} size={16} /></span>
        <div className="min-w-0 flex-1">
          <div className="net-point-title">{info.title}</div>
          <div className="truncate text-xs text-muted">{info.sub}</div>
        </div>
        {info.classification && (
          <Tooltip label={info.classification.detail ? `Source: ${info.classification.detail}` : "IP type"} openDelay={300} disabled={!info.classification.detail}>
            <span className="net-point-badge">{info.classification.label}</span>
          </Tooltip>
        )}
      </div>
      {info.error && (
        <div className="net-exit-error" role="alert">
          <IconAlertTriangle size={13} />
          <span>{info.error}</span>
        </div>
      )}
      {(info.ip || info.loading || info.isp || info.place) && (
        <dl className="net-point-facts">
          {info.ip ? (
            <Fact label="Address"><Address key={info.ip} ip={info.ip} privacy={info.privacy} /></Fact>
          ) : info.loading && (
            <Fact label="Address"><span className="flex items-center gap-1.5 text-faint"><Loader size={12} />Looking it up…</span></Fact>
          )}
          {info.isp && (
            <Fact label="Network">
              <span className="flex min-w-0 items-center gap-2">
                <span className="truncate" title={info.isp}>{info.isp}</span>
                {info.asn != null && <span className="mono shrink-0 text-xs text-faint">AS{info.asn}</span>}
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
