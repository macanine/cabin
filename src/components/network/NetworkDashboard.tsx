import { useAutoRefresh } from "../../lib/useAutoRefresh";
import { Button, Loader, Select, Switch, Tooltip } from "@mantine/core";
import { IconAlertTriangle, IconInfoCircle, IconPlus, IconRoute, IconTransfer } from "@tabler/icons-react";
import { useCallback, useMemo, useState } from "react";
import type { GeoInfo } from "../../services/ipc/generated/core";
import type { SystemRoute } from "../../services/ipc/generated/common";
import { interfaceOptions, normalizeRoute, type NetworkInterface, type NetworkRoute } from "../../lib/networkRoute";
import { toIpcError } from "../../services/ipc/common";
import { exitIpTypeLabel } from "../../lib/networkSummary";
import { networkIpc } from "../../services/ipc/network";
import { patchState, useApp } from "../../state/store";
import { uiActions } from "../../state/ui";
import { attempt } from "../../lib/operations";
import { flag, flagInName, hopRole, insertAt, linkStates, placeLabel, resolveHopPlaces, sameRoute } from "../../lib/geo";
import { RecoveryPanel } from "../Recovery";
import { NodeDrawer } from "./NodeDrawer";
import { NetworkGateSummary } from "./NetworkGatePanel";
import { PointCard, type PointInfo } from "./PointCard";
import { bareName, RoutePipeline } from "./RoutePipeline";
import { useNetworkLive } from "./useNetworkLive";
import { useNodeTests } from "./useNodeTests";
import { WorldMap } from "./WorldMap";
import { mapRoute, type MapPoint } from "./mapRoute";
import { groupNodes, type NodeGroup } from "./nodeCatalog";

const SYSTEM_ROUTE = ":system";

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
  const status = useApp((state) => state.network);
  const machineConnected = useApp((state) => state.machine?.state === "running" && state.machine.connection === "connected");
  const catalog = useApp((state) => state.proxy);
  const [edited, setEdited] = useState<NetworkRoute | null>(null);
  const [drawer, setDrawer] = useState(false);
  const [filter, setFilter] = useState<NodeGroup | null>(null);
  const [applying, setApplying] = useState(false);
  const [panelPoint, setPanelPoint] = useState<"origin" | "exit">("exit");
  const [selection, setSelection] = useState<string>("exit");
  const [interfaces, setInterfaces] = useState<NetworkInterface[] | null>(null);
  const [interfaceError, setInterfaceError] = useState<string | null>(null);
  const [systemRoute, setSystemRoute] = useState<SystemRoute | null>(null);
  const tests = useNodeTests();
  const active = useMemo(() => normalizeRoute(status.route), [status.route]);

  useAutoRefresh(async (isCurrent) => {
    try {
      const [found, route] = await Promise.all([networkIpc.interfaces(), networkIpc.systemRoute().catch(() => null)]);
      if (isCurrent()) {
        setInterfaces(found);
        setSystemRoute(route);
        setInterfaceError(null);
      }
    } catch (error) {
      if (isCurrent()) setInterfaceError(toIpcError(error).message);
    }
  }, { interval: 10_000, refreshKey: `${status.gatewaySession}:${status.generation}` });
  const dirty = edited != null && !sameRoute(edited, active);
  const draft = dirty ? edited : active;
  const selected = draft.chain.includes(selection) ? selection : panelPoint;
  const byId = useMemo(() => new Map(catalog.nodes.map((node) => [node.id, node])), [catalog.nodes]);
  const live = useNetworkLive(draft, byId);
  const group = filter && (catalog.sources.some((source) => `source:${source.id}` === filter)
    || catalog.groups.some((item) => `group:${item.id}` === filter)) ? filter : null;
  const candidates = useMemo(() => groupNodes(catalog, group), [catalog, group]);

  const { value: check, error: checkError, loading: checking } = live.check;
  const states = useMemo(() => linkStates(draft.chain.length, check, draft.chain), [draft.chain, check]);
  const brokenAt = check?.brokenAt ?? null;
  const exitGeo = live.exit.value;
  const originGeo = live.origin.value;
  const hopPlaces = useMemo(() => resolveHopPlaces(draft.chain, live.places), [draft.chain, live.places]);
  const mapHops = useMemo(() => draft.chain.map((id, index) => {
    const node = byId.get(id);
    const description = `Hop ${index + 1}, ${node ? bareName(node.name) : "missing node"}`;
    return point(hopPlaces.get(id)?.geo, "hop", id, description, String(index + 1));
  }), [draft.chain, byId, hopPlaces]);
  const { points, links } = useMemo(() => mapRoute(
    point(originGeo, "origin", "origin", "This Mac"), mapHops,
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
        sub: "This Mac",
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
      title: exitGeo ? placeLabel(exitGeo) : live.exit.loading ? "Looking up the exit" : "Location unknown",
      sub: `Observed exit · ${active.enabled ? "Active route" : "Direct"}`,
      classification: exitGeo ? {
        label: live.exitType.loading && !live.exitType.value ? "Checking IP type…" : exitIpTypeLabel(live.exitType.value?.ipType ?? null, live.exitType.value?.inferred),
        detail: live.exitType.value?.source ?? live.exitType.error,
      } : undefined,
      ip: exitGeo?.ip ?? null,
      privacy: true,
      isp: live.exitType.value?.organization ?? exitGeo?.isp ?? null,
      asn: live.exitType.value?.asn ?? exitGeo?.asn ?? null,
      place: placeOf(exitGeo),
      loading: live.exit.loading,
      error: live.exit.error,
    };
  }

  const hasHopPopover = points.some((item) => item.kind === "hop" && item.key === selected);
  const footerPoint = hasHopPopover ? panelPoint : selected;
  const options = interfaces ? interfaceOptions(interfaces, draft.bindInterface)
    : draft.bindInterface ? [{ value: draft.bindInterface, label: draft.bindInterface, disabled: true }] : [];
  const unavailableInterface = draft.bindInterface != null && interfaces != null && !interfaceError
    && !interfaces.some((item) => item.name === draft.bindInterface && item.available);
  const interfaceLabel = (name: string | null | undefined) =>
    interfaces?.find((item) => item.name === name)?.label || name || null;
  const tunnel = systemRoute?.tunnel ? systemRoute : null;
  const systemRouteLabel = tunnel?.internetInterface ? `System route · via ${tunnel.internetInterface}` : "System route";
  // The physical adapter under a tunnel, when it can be bound right now.
  const bypass = tunnel?.defaultInterface && interfaces?.some((item) => item.name === tunnel.defaultInterface && item.available)
    ? tunnel.defaultInterface : null;
  const activeInterface = active.bindInterface == null ? systemRouteLabel
    : interfaces?.find((item) => item.name === active.bindInterface)?.label || active.bindInterface;
  const interfaceOnly = dirty && sameRoute({ ...draft, bindInterface: active.bindInterface }, active);
  const deferred = !machineConnected || status.paused;
  const chainProblem = draft.enabled && draft.chain.length === 0 ? "Add a node to the route, or turn routing off."
    : draft.enabled && draft.chain.some((id) => !byId.has(id)) ? "Remove missing nodes before applying the route." : null;
  const blocked = !status.available ? "This machine has no network gateway."
    : unavailableInterface ? "The selected interface is unavailable. Choose another interface or the system route."
      : dirty && deferred ? interfaceOnly ? null : "Start the machine and resume internet access to apply proxy route changes."
        : chainProblem;
  const activePath = active.enabled ? active.chain.map((id) => bareName(byId.get(id)?.name ?? "Missing node")).join(" → ") || "No nodes" : "Direct";
  const gateway = !status.available ? "Gateway unavailable"
    : status.error ? "Gateway stopped"
      : !status.running ? "Machine stopped"
        : status.paused ? "Internet paused"
          : status.gate === "checking" ? "Verifying internet access"
            : status.gate === "closed" ? "Internet closed"
              : "Internet open";

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
      uiActions.toast(result.gate !== "open" ? "Interface saved" : interfaceOnly ? "Interface applied" : "Route applied", "success");
    }
    setApplying(false);
  }

  return (
    <>
      <div className="mb-3"><NetworkGateSummary status={status} /></div>
      <div className="net-card">
        <div className="net-stage">
          <WorldMap links={links} points={points} selected={selected} draft={dirty} onSelect={selectPoint}
            onDismiss={() => setSelection(panelPoint)} hopDetail={hasHopPopover && <PointCard info={pointInfo(selected)} />} />
          <div className="net-float net-float-controls">
            <div className="net-switch">
              <Switch
                size="sm"
                checked={draft.enabled}
                onChange={(event) => editRoute({ ...draft, enabled: event.currentTarget.checked })}
                label="Route via proxy"
                disabled={!status.available}
              />
            </div>
          </div>
          <div className="net-float net-float-actions">
            {draft.enabled && draft.chain.length > 0 && (
              <Tooltip label={dirty ? "Test draft chain" : "Test active chain"} openDelay={300}>
                <button type="button" className="net-float-button" onClick={() => void live.recheck()} disabled={checking || blocked != null} aria-label={dirty ? "Test draft chain" : "Test active chain"}>
                  {checking ? <Loader size={13} /> : <IconRoute size={15} />}
                </button>
              </Tooltip>
            )}
            <button type="button" className="net-float-button text" onClick={() => setDrawer((open) => !open)} aria-expanded={drawer}>
              <IconPlus size={14} /> Nodes
            </button>
          </div>
          <div className="net-map-status" aria-live="polite">
            {dirty && <span>Draft path</span>}
            {unmapped > 0 && <span>{unmapped} {unmapped === 1 ? "hop location" : "hop locations"} unknown</span>}
          </div>
          <div className="net-float-exit">
            <PointCard info={pointInfo(footerPoint)} />
          </div>
        </div>

        {status.error && (
          <div className="net-alert">
            <RecoveryPanel compact message={`The machine's network gateway is not running: ${status.error}`} />
          </div>
        )}

        <div className="net-route" id="network-route" tabIndex={-1}>
          <div className="net-route-current">
            <span className={`net-dot${gateway === "Internet open" ? " ok" : status.error || status.gate === "closed" && status.running && !status.paused ? " bad" : ""}`} aria-hidden />
            <strong>{status.gate === "open" ? "Active route" : "Saved route"}</strong>
            <span className="grow truncate" title={`${activePath} · ${activeInterface}`}>{activePath} · {activeInterface}</span>
            <span>{gateway}</span>
          </div>
          <div className="net-interface">
            <div className="flex flex-wrap items-end gap-2">
              <Select
                size="xs"
                className="w-full max-w-[300px]"
                label={(
                  <span className="inline-flex items-center gap-1">
                    Mac network interface
                    <Tooltip
                      multiline
                      w={300}
                      label="System route follows macOS routing, including VPNs and TUN proxies. A bound adapter sends the VM's traffic, DNS and the route's first hop out of that adapter only, bypassing any tunnel; internet access closes while it is disconnected."
                    >
                      <IconInfoCircle size={13} className="text-faint" aria-label="About interface binding" />
                    </Tooltip>
                  </span>
                )}
                data={[{ value: SYSTEM_ROUTE, label: systemRouteLabel }, ...options]}
                value={draft.bindInterface ?? SYSTEM_ROUTE}
                onChange={(value) => { if (value != null) editRoute({ ...draft, bindInterface: value === SYSTEM_ROUTE ? null : value }); }}
                allowDeselect={false}
                disabled={!status.available || applying}
              />
              {interfaceOnly && (
                <>
                  <Button size="xs" variant="subtle" disabled={applying} onClick={() => setEdited(null)}>Discard</Button>
                  <Button size="xs" loading={applying} disabled={blocked != null} onClick={() => void apply()}>
                    {deferred ? "Save interface" : "Apply interface"}
                  </Button>
                </>
              )}
            </div>
            {interfaceOnly && blocked && !unavailableInterface && <p className="net-interface-error" role="status">{blocked}</p>}
            {interfaceOnly && deferred && !blocked && <p className="mt-1.5 mb-0 text-xs text-faint">Checked when the machine is online.</p>}
            {tunnel && draft.bindInterface == null && (
              <div className="mt-2 flex flex-wrap items-center gap-2 rounded-lg border border-warn/30 bg-warn/8 px-3 py-2 text-sm" role="status">
                <IconTransfer size={15} className="shrink-0 text-warn" aria-hidden="true" />
                <span className="min-w-0 flex-1">
                  {tunnel.fakeIp ? "A TUN proxy (such as Clash)" : "A VPN"} on <span className="font-mono">{tunnel.internetInterface}</span> carries the system route.
                </span>
                {bypass && (
                  <Button size="compact-xs" variant="light" disabled={!status.available || applying}
                    onClick={() => editRoute({ ...draft, bindInterface: bypass })}>
                    Bind {interfaceLabel(bypass)}
                  </Button>
                )}
              </div>
            )}
            {interfaceError && <p className="net-interface-error" role="alert">Could not load interfaces: {interfaceError}</p>}
            {unavailableInterface && <p className="net-interface-error" role="status">The selected interface is unavailable. Choose another interface or the system route.</p>}
          </div>
          {dirty && !interfaceOnly && <div className="net-draft-title">Draft route</div>}
          <RoutePipeline
            chain={draft.chain}
            enabled={draft.enabled}
            draft={dirty}
            byId={byId}
            candidates={candidates}
            places={hopPlaces}
            check={check}
            links={states}
            tests={tests}
            bindInterface={draft.bindInterface}
            onChange={(chain) => editRoute({ ...draft, chain })}
            selected={selected}
            onSelect={selectPoint}
            exit={exitGeo ? { flag: flag(exitGeo.countryCode), label: exitGeo.city ?? exitGeo.country ?? exitGeo.countryCode ?? "Location unknown" } : null}
          />
          {draft.enabled && (brokenAt != null || checkError) && (
            <div className="net-chain-error" role="alert">
              <IconAlertTriangle size={13} />
              <span>{checkError ?? `Link ${brokenAt! + 1} is down · ${check?.hops[brokenAt!]?.detail ?? "Unreachable"}`}</span>
            </div>
          )}
          {!draft.enabled && draft.chain.length > 0 && <div className="net-chain-note">Routing is off. The saved chain stays available.</div>}
        </div>
      </div>

      {dirty && !interfaceOnly && (
        <div className="apply-bar net-apply">
          <div className="row">
            <div className="grow">
              {blocked ? <span className="bad fs-13">{blocked}</span> : (
                <span className="fs-13">{draft.enabled ? `Route through ${draft.chain.length} ${draft.chain.length === 1 ? "node" : "nodes"}` : "Send VM traffic directly from this Mac"}</span>
              )}
            </div>
            <Button variant="default" disabled={applying} onClick={() => setEdited(null)}>Discard draft</Button>
            <Button loading={applying} disabled={blocked != null} onClick={() => void apply()}>Apply route</Button>
          </div>
        </div>
      )}

      <NodeDrawer opened={drawer} onClose={() => setDrawer(false)} catalog={catalog} group={group} onGroup={setFilter}
        chain={draft.chain} tests={tests} bindInterface={draft.bindInterface} hostRefreshAllowed={status.gate === "closed"} onAdd={addNode} />
    </>
  );
}
