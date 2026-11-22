import { useSyncExternalStore } from "react";
import type { GeoInfo, IpClassification, NetworkStatus, NetworkTraffic, NodePlace, ProxyNode, RouteCheck } from "../services/ipc/generated/core";
import { toIpcError } from "../services/ipc/common";
import { networkIpc, proxyIpc } from "../services/ipc/network";
import { normalizeRoute, type NetworkRoute } from "../lib/networkRoute";
import { appStore } from "./store";
import { appendTraffic } from "../lib/trafficHistory";

export interface Lookup<T> {
  value: T | null;
  error: string | null;
  loading: boolean;
  /** When this was last measured (Mac clock), or null. */
  at: number | null;
}

export interface NetworkLive {
  exit: Lookup<GeoInfo>;
  exitType: Lookup<IpClassification>;
  origin: Lookup<GeoInfo>;
  places: ReadonlyMap<string, NodePlace>;
  refreshEvery: number;
}

interface GatewayScope {
  key: string;
  session: number;
}

const EMPTY: Lookup<never> = { value: null, error: null, loading: false, at: null };
const SUBSCRIPTION_MAX_AGE_MS = 30 * 60_000;
const SUBSCRIPTION_RETRY_MS = 10 * 60_000;
const REFRESH_KEY = "cabin.exitRefresh";
export const REFRESH_CHOICES = [0, 60, 300, 900] as const;
const PLACE_RETRY_MS = 2 * 60_000;
const TRAFFIC_SAMPLE_MS = 1_000;
const NO_TRAFFIC: readonly NetworkTraffic[] = [];

function readRefresh(): number {
  try {
    const raw = localStorage.getItem(REFRESH_KEY);
    const value = Number(raw);
    return raw != null && (REFRESH_CHOICES as readonly number[]).includes(value) ? value : 300;
  } catch {
    return 300;
  }
}

let state: NetworkLive = {
  exit: EMPTY,
  exitType: EMPTY,
  origin: EMPTY,
  places: new Map(),
  refreshEvery: readRefresh(),
};
let traffic: readonly NetworkTraffic[] = NO_TRAFFIC;
let scope: GatewayScope | null = null;
let trafficRequest: GatewayScope | null = null;
let exitEpoch = 0;
let exitPhase: "closed" | "checking" | "open" | null = null;
let observedNetwork: NetworkStatus | null = null;
let observedRunning = false;
let catalogNodes: readonly ProxyNode[] | null = null;
let nodeRevisions = new Map<string, string>();
const listeners = new Set<() => void>();
const placeTried = new Map<string, number>();

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

function set(patch: Partial<NetworkLive>) {
  state = { ...state, ...patch };
  listeners.forEach((listener) => listener());
}

function setTraffic(sample: NetworkTraffic | null) {
  const next = sample ? appendTraffic(traffic, sample) : NO_TRAFFIC;
  if (next === traffic) return;
  traffic = next;
  listeners.forEach((listener) => listener());
}

export function useNetworkMonitor(): NetworkLive {
  return useSyncExternalStore(subscribe, () => state);
}

export function useGatewayTraffic(): readonly NetworkTraffic[] {
  return useSyncExternalStore(subscribe, () => traffic);
}

export function useExitType(): Lookup<IpClassification> {
  return useSyncExternalStore(subscribe, () => state.exitType);
}

export function useExitMonitor(): Lookup<GeoInfo> {
  return useSyncExternalStore(subscribe, () => state.exit);
}

function located(place: NodePlace): boolean {
  return place.geo != null && (place.geo.countryCode != null || place.geo.latitude != null);
}

function syncCatalog(nodes: readonly ProxyNode[]) {
  if (nodes === catalogNodes) return;
  catalogNodes = nodes;
  const revisions = new Map(nodes.map((node) => [node.id, node.configRevision]));
  const changed = [...nodeRevisions.keys()].filter((id) => revisions.get(id) !== nodeRevisions.get(id));
  nodeRevisions = revisions;
  if (!changed.length) return;
  const places = new Map(state.places);
  for (const id of changed) {
    placeTried.delete(id);
    places.delete(id);
  }
  set({ places });
}

export async function refreshExit(refreshType = false): Promise<void> {
  if (!scope || exitPhase === "checking" || state.exit.loading || state.exitType.loading) return;
  const epoch = ++exitEpoch;
  set({ exit: { ...state.exit, loading: true }, exitType: { ...state.exitType, loading: true } });
  let observed: GeoInfo;
  try {
    observed = await networkIpc.exit();
  } catch (error) {
    if (epoch === exitEpoch) set({ exit: { ...state.exit, error: toIpcError(error).message, loading: false, at: Date.now() },
      exitType: { ...state.exitType, loading: false } });
    return;
  }
  if (epoch !== exitEpoch) return;
  const previousType = observed.ip === state.exit.value?.ip ? state.exitType.value : null;
  set({ exit: { value: observed, error: null, loading: false, at: Date.now() }, exitType: { value: previousType, error: null, loading: true, at: null } });
  try {
    const value = await networkIpc.exitType(observed.ip, refreshType);
    if (epoch === exitEpoch) set({ exitType: { value, error: null, loading: false, at: Date.now() } });
  } catch (error) {
    if (epoch === exitEpoch) set({ exitType: { value: previousType, error: toIpcError(error).message, loading: false, at: Date.now() } });
  }
}

async function sampleTraffic() {
  const current = scope;
  if (!current || document.hidden || trafficRequest === current) return;
  trafficRequest = current;
  try {
    const sample = await networkIpc.traffic();
    if (current === scope && !document.hidden) setTraffic(sample?.gatewaySession === current.session ? sample : null);
  } catch {
    if (current === scope) setTraffic(null);
  } finally {
    if (trafficRequest === current) trafficRequest = null;
  }
}

async function refreshOrigin(): Promise<void> {
  if (state.origin.value || state.origin.loading) return;
  set({ origin: { ...state.origin, loading: true } });
  try {
    set({ origin: { value: await networkIpc.origin(), error: null, loading: false, at: Date.now() } });
  } catch (error) {
    set({ origin: { value: null, error: toIpcError(error).message, loading: false, at: Date.now() } });
  }
}

/** Locate nodes whose current configuration has no location yet. */
export async function locate(ids: readonly string[]): Promise<void> {
  const revisions = nodeRevisions;
  const now = Date.now();
  const missing = [...new Set(ids)].filter((id) => {
    const known = state.places.get(id);
    return revisions.has(id) && !(known && located(known)) && now - (placeTried.get(id) ?? 0) > PLACE_RETRY_MS;
  });
  if (!missing.length) return;
  for (const id of missing) placeTried.set(id, now);
  try {
    const found = await networkIpc.locate(missing);
    const places = new Map(state.places);
    for (const place of found) {
      if (nodeRevisions.get(place.nodeId) !== revisions.get(place.nodeId)) continue;
      places.set(place.nodeId, place);
      if (located(place)) placeTried.delete(place.nodeId);
    }
    set({ places });
  } catch {
    // Retried after PLACE_RETRY_MS.
  }
}

/** An explicit request to check a chain link by link. */
export function checkChain(route: NetworkRoute): Promise<RouteCheck> {
  return networkIpc.check(route);
}

export function setRefreshEvery(seconds: number) {
  try { localStorage.setItem(REFRESH_KEY, String(seconds)); } catch { /* not remembered */ }
  set({ refreshEvery: seconds });
  scheduleExit();
}

let started = false;
let exitTimer: number | undefined;

let refreshingSource = false;
const subscriptionAttempts = new Map<string, number>();

async function refreshSubscription() {
  if (refreshingSource || document.hidden) return;
  const { network, proxy } = appStore.get();
  if (!network.available || !network.running || network.paused || network.gate !== "open" || network.connectivity !== "normal") return;
  const now = Date.now();
  const sourceIds = new Set(proxy.sources.map(({ id }) => id));
  for (const id of subscriptionAttempts.keys()) if (!sourceIds.has(id)) subscriptionAttempts.delete(id);
  const source = proxy.sources.find(({ id, hasUrl, updatedAt }) => hasUrl
    && now - updatedAt >= SUBSCRIPTION_MAX_AGE_MS
    && now - (subscriptionAttempts.get(id) ?? 0) >= SUBSCRIPTION_RETRY_MS);
  if (!source) return;
  refreshingSource = true;
  subscriptionAttempts.set(source.id, now);
  try { await proxyIpc.refresh(source.id); }
  catch { /* Keep the saved nodes and try again after the cooldown. */ }
  finally { refreshingSource = false; }
}

function refreshVisibleNetwork() {
  if (document.hidden) return;
  if (state.exit.at == null || Date.now() - state.exit.at >= 30_000) void refreshExit();
  void refreshSubscription();
}

function scheduleExit() {
  window.clearInterval(exitTimer);
  if (state.refreshEvery > 0) exitTimer = window.setInterval(() => { if (!document.hidden) void refreshExit(); }, state.refreshEvery * 1000);
}

function syncGateway() {
  const { machine, network, proxy } = appStore.get();
  const { gatewaySession, route } = network;
  const running = machine?.state === "running" && network.available && network.running && gatewaySession != null;
  if (observedNetwork === network && observedRunning === running && catalogNodes === proxy.nodes) return;
  observedNetwork = network;
  observedRunning = running;
  syncCatalog(proxy.nodes);
  const chain = route.enabled ? route.chain : [];
  const key = running ? JSON.stringify([gatewaySession, route.enabled, normalizeRoute(route).bindInterface, chain.map((id) => [id, nodeRevisions.get(id)])]) : null;
  const phase = key == null ? null : network.connectivity === "rechecking" ? "checking" : network.gate;
  const changedScope = key !== (scope?.key ?? null);
  const changedExit = changedScope || phase !== exitPhase;
  if (!changedExit) return;

  if (changedScope) {
    scope = key != null && gatewaySession != null ? { key, session: gatewaySession } : null;
    setTraffic(null);
  }
  exitPhase = phase;
  exitEpoch++;
  set({ exit: EMPTY, exitType: EMPTY });
  void refreshExit();
  if (scope && changedScope) {
    void sampleTraffic();
    void refreshOrigin();
    void locate(chain);
  }
}

export function startNetworkMonitor() {
  if (started) return;
  started = true;
  appStore.subscribe(syncGateway);
  syncGateway();
  scheduleExit();
  window.setInterval(() => void sampleTraffic(), TRAFFIC_SAMPLE_MS);
  window.setInterval(() => void refreshSubscription(), 60_000);
  window.addEventListener("focus", refreshVisibleNetwork);
  refreshVisibleNetwork();
  document.addEventListener("visibilitychange", () => {
    if (document.hidden) setTraffic(null);
    else { void sampleTraffic(); refreshVisibleNetwork(); }
  });
  window.setInterval(() => {
    const { route } = appStore.get().network;
    if (scope && route.enabled) void locate(route.chain);
  }, PLACE_RETRY_MS + 1000);
}
