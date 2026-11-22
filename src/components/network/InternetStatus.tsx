import { Popover, Tooltip } from "@mantine/core";
import { IconArrowDown, IconArrowUp, IconPlayerPause, IconWorld, IconWorldOff } from "@tabler/icons-react";
import { formatRate } from "../../lib/format";
import { flag, placeLabel } from "../../lib/geo";
import { exitIpTypeLabel, selectNetworkSummary, type NetTone } from "../../lib/networkSummary";
import { useExitMonitor, useExitType, useGatewayTraffic } from "../../state/networkMonitor";
import { useApp } from "../../state/store";
import { uiActions } from "../../state/ui";
import { Flag } from "../Flag";
import { PointCard } from "./PointCard";

function Rates() {
  const history = useGatewayTraffic();
  const sample = history[history.length - 1];
  if (!sample) return null;
  return (
    <span className="flex shrink-0 flex-col items-end text-xs leading-tight text-muted tabular-nums" aria-label="Network speed">
      <span className="flex items-center gap-0.5"><IconArrowDown size={10} aria-hidden />{formatRate(sample.rxBps)}</span>
      <span className="flex items-center gap-0.5"><IconArrowUp size={10} aria-hidden />{formatRate(sample.txBps)}</span>
    </span>
  );
}

const ICON = { ok: IconWorld, warn: IconPlayerPause, bad: IconWorldOff, off: IconWorldOff, busy: IconWorld } as const;
const TONE: Record<NetTone, string> = { ok: "text-muted", busy: "animate-breathe text-muted", warn: "text-warn", bad: "text-bad", off: "text-faint" };

function SideMark({ tone, country }: { tone: NetTone; country: string | null }) {
  const Icon = ICON[tone];
  return (
    <span className={`grid size-5 shrink-0 place-items-center ${TONE[tone]}`} aria-hidden>
      {country ? <Flag code={country} size={12} /> : <Icon size={16} stroke={1.7} />}
    </span>
  );
}

export function InternetStatus({ collapsed }: { collapsed: boolean }) {
  const summary = useApp(selectNetworkSummary);
  const exit = useExitMonitor();
  const type = useExitType();
  const place = exit.value ? placeLabel(exit.value) : null;
  const country = flag(exit.value?.countryCode);
  const ipType = type.loading && !type.value ? "Checking type…" : exitIpTypeLabel(type.value?.ipType ?? null, type.value?.inferred);
  const detail = [summary.detail, place, exit.value ? ipType : null].filter(Boolean).join(" · ");
  const label = `Network Check. ${summary.title}. ${detail}`;
  const mark = <SideMark tone={summary.tone} country={country} />;

  if (collapsed) {
    return (
      <Tooltip label={`${summary.title} · ${detail}`} position="right" multiline maw={260} events={{ hover: true, focus: true, touch: false }}>
        <button type="button" className="press mx-auto mt-1 grid size-8 cursor-pointer place-items-center rounded-lg border-0 bg-transparent p-0 hover:bg-hover" onClick={uiActions.openNetworkCheck} aria-label={label} aria-haspopup="dialog">
          {mark}
        </button>
      </Tooltip>
    );
  }

  return (
    <div className="relative min-w-0">
      <button type="button" className="press grid w-full cursor-pointer grid-cols-[20px_minmax(0,1fr)_auto] items-center gap-2.5 rounded-t-[var(--card-radius)] border-0 bg-transparent py-1.5 pr-2 pl-[7px] text-left text-fg hover:bg-hover"
        onClick={uiActions.openNetworkCheck} aria-label={label} aria-haspopup="dialog" title={detail}>
        {exit.value ? <span aria-hidden /> : mark}
        <span className="grid min-w-0">
          <span className={`truncate text-sm font-medium ${summary.tone === "bad" ? "text-bad" : ""}`}>{summary.tone === "ok" && place ? place : summary.title}</span>
          <span className="truncate text-xs text-muted">{summary.detail}</span>
        </span>
        <Rates />
      </button>
      {exit.value && (
        <Popover position="right" width={286} withinPortal trapFocus returnFocus>
          <Popover.Target>
            <button type="button" className="press absolute top-1/2 left-[7px] grid size-5 -translate-y-1/2 cursor-pointer place-items-center rounded border-0 bg-transparent p-0 hover:bg-sel"
              aria-label={`Exit IP details, ${place ?? "unknown location"}, ${ipType}`}>
              {mark}
            </button>
          </Popover.Target>
          <Popover.Dropdown>
            <PointCard info={{
              title: place ?? "Exit IP", sub: `Observed exit · ${exit.value.source}`, flag: country,
              ip: exit.value.ip, privacy: true, isp: type.value?.organization ?? exit.value.isp,
              asn: type.value?.asn ?? exit.value.asn, loading: exit.loading, error: exit.error, place: null,
              classification: { label: ipType, detail: type.value?.source ?? type.error },
            }} />
          </Popover.Dropdown>
        </Popover>
      )}
    </div>
  );
}
