import { Button, Loader, Select, Switch, Tooltip } from "@mantine/core";
import {
  IconAlertTriangle, IconArrowRight, IconDeviceLaptop, IconLayoutGrid,
  IconPlayerPause, IconPlayerPlay, IconRefresh, IconRoute, IconServer,
  IconShieldCheck, IconShieldLock, IconWorld,
} from "@tabler/icons-react";
import { useCallback, useMemo, useState } from "react";
import type { GeoInfo } from "../../services/ipc/generated/core";
import { normalizeRoute, type NetworkRoute } from "../../lib/networkRoute";
import { checkFix, exitIpTypeLabel, networkGuidance, selectNetworkSummary } from "../../lib/networkSummary";
import { confirmAction } from "../../services/ipc/dialogs";
import { networkIpc } from "../../services/ipc/network";
import { patchState, useApp } from "../../state/store";
import { uiActions } from "../../state/ui";
import { attempt } from "../../lib/operations";
import { flag, flagInName, hopRole, insertAt, linkStates, placeLabel, resolveHopPlaces, sameRoute } from "../../lib/geo";
import { RecoveryPanel } from "../Recovery";
import { SettingRow } from "../common";
import { NodeDrawer } from "./NodeDrawer";
import { NetworkCheckButton } from "./NetworkGatePanel";
import { PointCard, type PointInfo } from "./PointCard";
import { bareName, RoutePipeline } from "./RoutePipeline";
import { useNetworkLive } from "./useNetworkLive";
import { WorldMap } from "./WorldMap";
import { mapRoute, type MapPoint } from "./mapRoute";
import { useI18n } from "../../i18n";

function placeOf(geo: GeoInfo | null | undefined): PointInfo["place"] {
  if (geo?.latitude == null || geo.longitude == null) return null;
  if (!Number.isFinite(geo.latitude) || !Number.isFinite(geo.longitude) || Math.abs(geo.latitude) > 90 || Math.abs(geo.longitude) > 180) return null;
  return { lat: geo.latitude, lon: geo.longitude, timeZone: geo.timezone };
}

function point(geo: GeoInfo | null | undefined, kind: MapPoint["kind"], key: string, description: string, label?: string): MapPoint | null {
  const place = placeOf(geo);
  return place ? { lat: place.lat, lon: place.lon, kind, key, label, description } : null;
}

export function NetworkDashboard() {
  const { t } = useI18n();
  const status = useApp((state) => state.network);
  const summary = useApp(selectNetworkSummary);
  const machineConnected = useApp((state) => state.machine?.state === "running" && state.machine.connection === "connected");
  const catalog = useApp((state) => state.proxy);
  const [edited, setEdited] = useState<NetworkRoute | null>(null);
  const [drawer, setDrawer] = useState(false);
  const [applying, setApplying] = useState(false);
  const [gateBusy, setGateBusy] = useState<"verify" | "accept" | "pause" | null>(null);
  const [panelPoint, setPanelPoint] = useState<"origin" | "exit">("exit");
  const [selection, setSelection] = useState<string>("exit");
  const active = useMemo(() => normalizeRoute(status.route), [status.route]);

  const dirty = edited != null && !sameRoute(edited, active);
  const draft = dirty ? edited : active;
  const selected = draft.chain.includes(selection) ? selection : panelPoint;
  const byId = useMemo(() => new Map(catalog.nodes.map((node) => [node.id, node])), [catalog.nodes]);
  const live = useNetworkLive(draft, byId);
  const { value: check, error: checkError, loading: checking } = live.check;
  const states = useMemo(() => linkStates(draft.chain.length, check, draft.chain), [draft.chain, check]);
  const brokenAt = check?.brokenAt ?? null;
  const exitGeo = live.exit.value;
  const exitType = live.exitType.value;
  const originGeo = live.origin.value;
  const hopPlaces = useMemo(() => resolveHopPlaces(draft.chain, live.places), [draft.chain, live.places]);
  const mapHops = useMemo(() => draft.chain.map((id, index) => {
    const node = byId.get(id);
    const description = `Hop ${index + 1}, ${node ? bareName(node.name) : "missing node"}`;
    return point(hopPlaces.get(id)?.geo, "hop", id, description, String(index + 1));
  }), [draft.chain, byId, hopPlaces]);
  const { points, links } = useMemo(() => mapRoute(
    point(originGeo, "origin", "origin", "This computer"), mapHops,
    point(exitGeo, "exit", "exit", "Observed exit"), states, draft.enabled, dirty,
  ), [originGeo, mapHops, exitGeo, states, draft.enabled, dirty]);
  const unmapped = draft.enabled ? mapHops.filter((hop) => hop == null).length : 0;

  function selectPoint(key: string) {
    if (key === "origin" || key === "exit") setPanelPoint(key);
    setSelection(key);
  }

  function pointInfo(key: string): PointInfo {
    if (key === "origin") {
      return {
        flag: flag(originGeo?.countryCode),
        title: originGeo ? placeLabel(originGeo) : "Location unknown",
        sub: "This computer",
        ip: originGeo?.ip ?? null,
        privacy: true,
        isp: originGeo?.isp ?? null,
        asn: originGeo?.asn ?? null,
        place: placeOf(originGeo),
        loading: live.origin.loading,
        error: live.origin.error,
      };
    }
    const index = draft.chain.indexOf(key);
    if (index >= 0) {
      const node = byId.get(key);
      const place = hopPlaces.get(key);
      const where = place?.geo ? placeLabel(place.geo) : "Location unknown";
      return {
        flag: flag(place?.geo?.countryCode) ?? (node ? flagInName(node.name) : null),
        title: node ? bareName(node.name) : "Missing node",
        sub: `${dirty ? "Draft · " : ""}${hopRole(index, draft.chain.length)} · ${where}`,
        ip: place?.geo?.ip ?? place?.host ?? null,
        privacy: false,
        isp: place?.geo?.isp ?? null,
        asn: place?.geo?.asn ?? null,
        place: placeOf(place?.geo),
        loading: false,
        error: null,
      };
    }
    return {
      flag: flag(exitGeo?.countryCode),
      title: placeLabel(exitGeo, "Public exit"),
      sub: "Observed exit",
      classification: exitType && exitType.ipType !== "unknown" ? {
        label: exitIpTypeLabel(exitType.ipType, exitType.inferred),
        detail: exitType.source,
      } : undefined,
      ip: exitGeo?.ip ?? null,
      privacy: true,
      isp: live.exitType.value?.organization ?? exitGeo?.isp ?? null,
      asn: live.exitType.value?.asn ?? exitGeo?.asn ?? null,
      place: placeOf(exitGeo),
      loading: live.exit.loading,
      error: live.exit.error ? exitGeo ? "Couldn’t refresh the exit. Showing the last observation." : "Couldn’t read the public exit. Try again in a moment." : null,
    };
  }

  const hasHopPopover = points.some((item) => item.kind === "hop" && item.key === selected);
  const footerPoint = hasHopPopover ? panelPoint : selected;
  const deferred = !machineConnected || status.paused;
  const chainProblem = draft.enabled && draft.chain.length === 0 ? "Add at least one proxy node, or turn off proxy routing."
    : draft.enabled && draft.chain.some((id) => !byId.has(id)) ? "Remove missing nodes before applying the route." : null;
  const blocked = !status.available ? "This machine has no network gateway."
    : dirty && deferred ? "Start the machine and resume internet access to apply route changes."
      : chainProblem;
  const activePath = active.enabled
    ? active.chain.map((id) => bareName(byId.get(id)?.name ?? "Missing node")).join(" → ") || t("network.noNodes")
    : t("network.directViaComputer");
  const policyLabel = (active.exitPolicy ?? "static") === "dynamicResidential" ? t("network.exitPolicyDynamic") : t("network.staticPolicy");

  const addNode = useCallback((id: string) => setEdited((old) => {
    const route = old ?? active;
    if (route.chain.includes(id)) return old;
    return { ...route, chain: insertAt(route.chain, route.chain.length, id) };
  }), [active]);

  function editRoute(next: NetworkRoute) {
    setEdited(sameRoute(next, active) ? null : next);
  }

  async function apply() {
    const submitted = draft;
    setApplying(true);
    const result = await attempt(networkIpc.apply(submitted));
    if (result) {
      patchState((state) => state.network.generation > result.generation ? state : { ...state, network: result });
      setEdited((current) => current && sameRoute(current, submitted) ? null : current);
      uiActions.toast("Route applied", "success");
    }
    setApplying(false);
  }

  async function runGate(action: "verify" | "accept" | "pause") {
    if (action === "accept" && !await confirmAction(
      "Replace this route’s approved exit if the current exit passes a full network check? Only continue if you expected the change.",
      "Approve and check", "Approve new exit",
    )) return;
    setGateBusy(action);
    try {
      if (action === "verify" || action === "accept") await attempt(networkIpc.verify(action === "accept"));
      else await attempt(networkIpc.setPaused(!status.paused));
    } finally {
      setGateBusy(null);
    }
  }

  const holding = status.connectivity === "rechecking";
  const canChangeGate = machineConnected && status.available && (status.running || status.paused) && gateBusy == null;
  const canVerify = canChangeGate && !status.paused && status.gate !== "checking" && !holding;
  const gateChecking = gateBusy === "verify" || gateBusy === "accept" || summary.tone === "busy";
  const needsGuidance = machineConnected && status.available && (summary.tone === "bad" || status.paused || holding);
  const guidance = networkGuidance(status, summary);
  const blockerFix = summary.blocker ? checkFix(summary.blocker) : null;
  const bannerTone = holding ? "info" : status.paused ? "warn" : "bad";
  const detailLine = holding
    ? [summary.detail, status.connectivityError].filter(Boolean).join(" · ")
    : status.exitPolicyError ?? (summary.tone === "bad" && summary.detail !== guidance.headline ? summary.detail : null);

  function jumpTo(target: "route" | "region" | "browser") {
    if (target === "browser") uiActions.openBrowserEnvironment();
    else {
      const el = document.getElementById(target === "route" ? "network-route" : "network-region");
      el?.scrollIntoView({ behavior: "smooth", block: "start" });
      el?.focus({ preventScroll: true });
    }
  }

  return (
    <div className="grid gap-5">
      {/* 1. Connection status — becomes an inline guided recovery banner whenever held, blocked, or paused */}
      {needsGuidance ? (
        <section className={`net-guide-card ${bannerTone}`} role={holding ? "status" : "alert"} aria-label="Network status">
          <div className="net-guide-head">
            <span className="net-guide-icon" aria-hidden>
              {holding ? <Loader size={14} /> : status.paused ? <IconPlayerPause size={16} /> : <IconShieldLock size={16} />}
            </span>
            <div className="min-w-0 flex-1">
              <strong className="block text-[15px] leading-snug font-semibold text-fg">{guidance.headline}</strong>
              <p className="m-0 mt-1 text-sm leading-relaxed text-muted">{guidance.explanation}</p>
            </div>
            <NetworkCheckButton />
          </div>
          {detailLine && <div className="net-guide-detail mono">{detailLine}</div>}
          {!holding && (
            <div className="flex flex-wrap items-center gap-2 pt-0.5">
              {status.paused ? (
                <Button size="xs" leftSection={<IconPlayerPlay size={14} />} disabled={!canChangeGate} loading={gateBusy === "pause"} onClick={() => void runGate("pause")}>
                  {t("network.resumeInternet")}
                </Button>
              ) : status.exitPolicyError ? (
                <>
                  <Button size="xs" disabled={!canVerify} loading={gateBusy === "accept"} onClick={() => void runGate("accept")}>
                    {t("network.approveNewExit")}
                  </Button>
                  <Button size="xs" variant="default" disabled={!canVerify} loading={gateBusy === "verify"} onClick={() => void runGate("verify")}>
                    {t("network.checkAgain")}
                  </Button>
                </>
              ) : (
                <>
                  <Button size="xs" leftSection={<IconRefresh size={14} />} disabled={!canVerify} loading={gateBusy === "verify"} onClick={() => void runGate("verify")}>
                    {t("network.checkAgain")}
                  </Button>
                  {blockerFix && blockerFix.target !== "route" && (
                    <Button size="xs" variant="default" onClick={() => jumpTo(blockerFix.target)}>{blockerFix.label}</Button>
                  )}
                </>
              )}
            </div>
          )}
          {guidance.tip && <p className="m-0 text-xs leading-relaxed text-faint">{guidance.tip}</p>}
        </section>
      ) : (
        <section className="net-card net-head" aria-label="Network status">
          <span className={`net-status-badge ${gateChecking ? "busy" : summary.tone === "ok" ? "ok" : ""}`} aria-hidden>
            {gateChecking ? <Loader size={13} /> : <IconShieldCheck size={16} />}
          </span>
          <div className="net-head-copy" role="status">
            <strong>{gateChecking ? t("network.verifying") : summary.tone === "ok" ? t("network.internetOpen") : !machineConnected ? t("network.machineOffline") : t("network.gatewayReady")}</strong>
            <span className="truncate" title={`${activePath} · ${policyLabel}`}>{activePath} · {policyLabel}</span>
          </div>
          <div className="flex flex-wrap items-center gap-1.5">
            {machineConnected && status.available && (
              <>
                <Button size="xs" variant="default" leftSection={<IconRefresh size={13} />} disabled={!canVerify} loading={gateBusy === "verify"} onClick={() => void runGate("verify")}>
                  {t("network.checkNow")}
                </Button>
                <Button size="xs" variant="subtle" color="gray" disabled={!canChangeGate} loading={gateBusy === "pause"} onClick={() => void runGate("pause")}>
                  {t("network.pause")}
                </Button>
              </>
            )}
            <NetworkCheckButton />
          </div>
        </section>
      )}

      {status.error && (
        <div className="net-card net-alert">
          <RecoveryPanel compact message={`The machine's network gateway is not running: ${status.error}`} />
        </div>
      )}

      {/* 2. Traffic route & IP protection */}
      <section className="net-section" id="network-route" tabIndex={-1} aria-label={t("network.trafficRoute")}>
        <header className="net-section-head">
          <div>
            <h3>{t("network.trafficRoute")}</h3>
            <p>{t("network.trafficRouteSubtitle")}</p>
          </div>
          <div className="flex items-center gap-1.5">
            {draft.enabled && draft.chain.length > 0 && (
              <Tooltip label={dirty ? "Test draft chain" : "Test active chain"} openDelay={300}>
                <button type="button" className="net-tool press" onClick={() => void live.recheck()} disabled={checking || blocked != null} aria-label={t("network.test")}>
                  {checking ? <Loader size={13} /> : <IconRoute size={15} />}
                  <span>{t("network.test")}</span>
                </button>
              </Tooltip>
            )}
            <button type="button" className="net-tool press" onClick={() => setDrawer((open) => !open)} aria-expanded={drawer}>
              <IconLayoutGrid size={15} />
              <span>{t("network.nodes")}{catalog.nodes.length ? ` (${catalog.nodes.length})` : ""}</span>
            </button>
          </div>
        </header>

        <div className="net-card">
          <SettingRow
            label={t("network.routeViaProxy")}
            hint={t("network.routeViaProxyHint")}
          >
            <Switch
              size="sm"
              checked={draft.enabled}
              onChange={(event) => editRoute({ ...draft, enabled: event.currentTarget.checked })}
              aria-label={t("network.routeViaProxy")}
              disabled={!status.available}
            />
          </SettingRow>

          {draft.enabled ? (
            <div className="net-route border-t border-line/70">
              <RoutePipeline
                chain={draft.chain}
                enabled={draft.enabled}
                draft={dirty}
                byId={byId}
                candidates={catalog.nodes}
                places={hopPlaces}
                check={check}
                links={states}
                onChange={(chain) => editRoute({ ...draft, chain })}
                selected={selected}
                onSelect={selectPoint}
              />
              {(brokenAt != null || checkError) && (
                <div className="net-chain-error" role="alert">
                  <IconAlertTriangle size={13} />
                  <span>{checkError ?? `Link ${brokenAt! + 1} is down · ${check?.hops[brokenAt!]?.detail ?? "Unreachable"}`}</span>
                </div>
              )}
            </div>
          ) : (
            <div className="net-direct-strip">
              <span className="net-direct-node"><IconServer size={14} /> {t("network.directStripVm")}</span>
              <IconArrowRight size={13} className="text-faint" aria-hidden />
              <span className="net-direct-node"><IconDeviceLaptop size={14} /> {t("network.directStripHost")}</span>
              <IconArrowRight size={13} className="text-faint" aria-hidden />
              <span className="net-direct-node"><IconWorld size={14} /> {t("network.directStripWeb")}</span>
            </div>
          )}

          <SettingRow
            label={t("network.exitPolicy")}
            hint={(draft.exitPolicy ?? "static") === "dynamicResidential"
              ? t("network.exitPolicyHintDynamic")
              : t("network.exitPolicyHintStatic")}
          >
            <Select
              size="xs"
              className="w-52 max-w-full"
              aria-label={t("network.exitPolicy")}
              data={[
                { value: "static", label: t("network.exitPolicyStatic") },
                { value: "dynamicResidential", label: t("network.exitPolicyDynamic") },
              ]}
              value={draft.exitPolicy ?? "static"}
              onChange={(value) => {
                if (value === "static" || value === "dynamicResidential") editRoute({ ...draft, exitPolicy: value });
              }}
              allowDeselect={false}
              disabled={!status.available || applying}
            />
          </SettingRow>

          {dirty && (
            <div className="flex min-w-0 flex-wrap items-center justify-end gap-2 border-t border-line bg-hover/40 px-4 py-3" data-route-actions>
              {blocked && <p className="m-0 min-w-0 basis-48 grow text-sm break-words text-bad" role="status">{blocked}</p>}
              <Button size="xs" variant="default" disabled={applying} onClick={() => setEdited(null)}>{t("common.discard")}</Button>
              <Button size="xs" loading={applying} disabled={blocked != null} onClick={() => void apply()}>
                {t("network.applyAndVerify")}
              </Button>
            </div>
          )}
        </div>
      </section>

      {/* 3. Live observed location & world map */}
      <section className="net-section" aria-label={t("network.liveLocation")}>
        <header className="net-section-head">
          <div>
            <h3>{t("network.liveLocation")}</h3>
            <p>{t("network.liveLocationSubtitle")}</p>
          </div>
        </header>
        <div className="net-card net-where">
          <div className="net-stage">
            <WorldMap links={links} points={points} selected={selected} draft={dirty} onSelect={selectPoint}
              onDismiss={() => setSelection(panelPoint)} hopDetail={hasHopPopover && <PointCard compact info={pointInfo(selected)} />} />
            <div className="net-map-status" aria-live="polite">
              {dirty && <span>{t("network.draftPath")}</span>}
              {unmapped > 0 && <span>{unmapped} {unmapped === 1 ? "hop location" : "hop locations"} unknown</span>}
            </div>
          </div>
          <div className="net-where-point">
            <PointCard
              info={pointInfo(footerPoint)}
              actions={
                <div className="net-point-switch" role="tablist" aria-label="Inspect route endpoint">
                  <button type="button" role="tab" aria-selected={footerPoint === "exit"} onClick={() => selectPoint("exit")}>
                    {t("network.exit")}
                  </button>
                  <button type="button" role="tab" aria-selected={footerPoint === "origin"} onClick={() => selectPoint("origin")}>
                    {t("network.thisComputer")}
                  </button>
                </div>
              }
            />
          </div>
        </div>
      </section>

      <NodeDrawer opened={drawer} onClose={() => setDrawer(false)} catalog={catalog}
        chain={draft.chain} hostRefreshAllowed={status.gate === "closed" && !holding} onAdd={addNode} />
    </div>
  );
}
