import { Loader, Tooltip } from "@mantine/core";
import {
  IconAlertTriangle, IconBuildingBroadcastTower, IconCheck, IconCopy, IconEye, IconEyeOff, IconMoon,
  IconServer, IconSun,
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
    <div className="net-exit-line net-ip">
      {privacy ? (
        <Tooltip label={VISIBILITY_LABEL[visibility]} openDelay={300}>
          <button className="net-icon-button" onClick={cycle} aria-label={VISIBILITY_LABEL[visibility]}>
            {visibility === "shown" ? <IconEye size={15} /> : <IconEyeOff size={15} />}
          </button>
        </Tooltip>
      ) : (
        <span className="net-line-icon"><IconServer size={15} /></span>
      )}
      <span className="net-ip-value mono" aria-label={visibility === "shown" ? ip : "Address, hidden"}>
        {maskIp(ip, visibility).map((part, index) => (
          <span key={index} className={part.blur ? "net-blur" : undefined}>{part.text}</span>
        ))}
      </span>
      <Tooltip label={copied ? "Copied" : "Copy the address"} openDelay={200}>
        <button className="net-icon-button" aria-label="Copy the address" onClick={() => void clipboard.write(ip).then(() => setCopied(true)).catch(() => undefined)}>
          {copied ? <IconCheck size={14} /> : <IconCopy size={14} />}
        </button>
      </Tooltip>
    </div>
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
    <div className="net-exit-line net-daylight" title={timeZone ? `Local time (${timeZone})` : "Local time"}>
      <span className={`net-line-icon ${light.day ? "sun" : "moon"}`}>{light.day ? <IconSun size={15} /> : <IconMoon size={15} />}</span>
      <span className="net-daylight-text">
        <strong className="mono">{clock(now, timeZone)}</strong>
        <span> · {light.polar ?? light.phase.split(",")[0]}</span>
      </span>
    </div>
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

/** One point of the route (this Mac, a node, the exit), always in full. */
export function PointCard({ info }: { info: PointInfo }) {
  return (
    <div className={`net-exit${info.error && !info.ip ? " failed" : ""}`}>
      <div className="net-exit-place">
        <span className="net-flag" aria-hidden><Flag code={info.flag} size={16} /></span>
        <div className="grow net-exit-title">
          <div className="net-exit-name">{info.title}</div>
          <div className="net-exit-sub">{info.sub}</div>
        </div>
      </div>
      {info.error && (
        <div className="net-exit-error" role="alert">
          <IconAlertTriangle size={13} />
          <span>{info.error}</span>
        </div>
      )}
      {info.ip ? <Address key={info.ip} ip={info.ip} privacy={info.privacy} /> : info.loading && (
        <div className="net-exit-line faint"><span className="net-line-icon"><Loader size={12} /></span>Looking it up…</div>
      )}
      {info.classification && <div className="net-exit-line net-ip-classification">
        <span>{info.classification.label}</span>
        {info.classification.detail && <span className="truncate faint" title={info.classification.detail}>{info.classification.detail}</span>}
      </div>}
      {info.isp && (
        <div className="net-exit-line net-isp">
          <span className="net-line-icon"><IconBuildingBroadcastTower size={15} /></span>
          <Tooltip label={info.asn != null ? `AS${info.asn}` : "No AS number reported"} openDelay={300}>
            <span className="truncate">{info.isp}</span>
          </Tooltip>
        </div>
      )}
      {info.place && <Daylight lat={info.place.lat} lon={info.place.lon} timeZone={info.place.timeZone} />}
    </div>
  );
}
