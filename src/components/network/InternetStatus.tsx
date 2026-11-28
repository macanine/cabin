import { IconArrowDown, IconArrowUp, IconPlayerPause, IconWorld, IconWorldOff } from "@tabler/icons-react";
import { formatRate } from "../../lib/format";
import { flag } from "../../lib/geo";
import { selectNetworkSummary } from "../../lib/networkSummary";
import { useExitMonitor, useGatewayTraffic } from "../../state/networkMonitor";
import { useApp } from "../../state/store";
import { uiActions, useUi } from "../../state/ui";
import { Flag } from "../Flag";
import { useI18n } from "../../i18n";

const ICON = { ok: IconWorld, warn: IconPlayerPause, bad: IconWorldOff, off: IconWorldOff, busy: IconWorld } as const;
const TONE = { ok: "text-muted", busy: "text-info", warn: "text-warn", bad: "text-bad", off: "text-faint" } as const;

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

function useInternetDisplay() {
  const { t } = useI18n();
  const { tone, title, detail } = useApp(selectNetworkSummary);
  const holding = useApp((state) => state.networkLoaded && state.network.connectivity === "rechecking");
  const { value, loading } = useExitMonitor();
  // The Network page already shows the observation in its exit card.
  const showExit = useUi(({ view }) => view.kind !== "settings" || view.tab !== "network");
  const countryCode = showExit ? flag(value?.countryCode) : null;
  const country = showExit ? countryCode || value?.country : null;
  const fullCountry = showExit ? value?.country || countryCode : null;
  const displayTitle = holding ? t("network.holding") : tone === "ok" ? t("network.internet") : title;
  const sub = holding ? detail : country;
  const label = sub ? `${displayTitle} · ${sub}` : displayTitle;
  const tooltip = holding ? `${displayTitle} · ${detail}` : fullCountry ? `${displayTitle} · ${fullCountry}` : displayTitle;
  return { tone, title: displayTitle, sub, holding, countryCode, pending: showExit && loading, label, tooltip };
}

type InternetDisplay = ReturnType<typeof useInternetDisplay>;

function StatusMark({ display, compact = false }: { display: InternetDisplay; compact?: boolean }) {
  const { tone, countryCode, pending, label } = display;
  const Icon = ICON[tone];
  return (
    <span className={`grid size-5 shrink-0 place-items-center ${TONE[tone]}`} role={compact ? "img" : undefined} aria-label={compact ? label : undefined} aria-hidden={compact ? undefined : true} title={compact ? label : undefined}>
      {countryCode || (tone === "busy" && pending)
        ? <Flag code={countryCode} size={12} pending={pending} />
        : <Icon size={16} stroke={1.7} aria-hidden />}
    </span>
  );
}

export function InternetMark() {
  return <StatusMark display={useInternetDisplay()} compact />;
}

export function InternetStatus() {
  const display = useInternetDisplay();
  const { tone, title, sub, label, tooltip } = display;
  return (
    <div className="grid h-9 min-w-0 grid-cols-[20px_minmax(0,1fr)_auto] items-center gap-2.5 pr-2 pl-[7px]">
      <StatusMark display={display} />
      <button type="button" className="press -ml-1 flex h-7 min-w-0 cursor-pointer items-center gap-1.5 rounded-md border-0 bg-transparent px-1 py-0 text-left text-sm font-medium hover:bg-hover"
        onClick={uiActions.openNetworkCheck} title={tooltip} aria-label={`Network check: ${label}`} aria-haspopup="dialog">
        <span className={`shrink-0 ${TONE[tone]}`}>{title}</span>
        {sub && <span className="truncate text-muted">· {sub}</span>}
      </button>
      {tone === "ok" && <Rates />}
    </div>
  );
}
