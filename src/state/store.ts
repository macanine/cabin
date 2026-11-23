import { reportFrontendDiagnostic } from "../services/ipc/common";
import { useRef, useSyncExternalStore } from "react";
import { noteChatState, noteOutput } from "./unread";
import type { AppEvent, HostSnapshot } from "../services/ipc/generated/core";
import { appIpc } from "../services/ipc/app";
import { toIpcError } from "../services/ipc/common";
import { networkIpc, proxyIpc } from "../services/ipc/network";
import { syncIpc } from "../services/ipc/sync";
import { startClockSync } from "./clockSync";
import { startNetworkMonitor } from "./networkMonitor";
import { type AppState, applyEvent, fromHostSnapshot, initialState } from "./reducer";
import type { TerminalEvent } from "../lib/terminalReplay";

type Listener = () => void;
type TerminalListener = (event: TerminalEvent) => void;
type ChatListener = (
  event: Extract<AppEvent, {
    type: "chatItem" | "chatPlan" | "chatSubagent" | "chatTask" | "chatState" | "chatModel" | "chatPermission" | "chatUsage" | "chatControls";
  }> | null,
) => void;

let state: AppState = initialState();
let connectError: string | null = null;
const listeners = new Set<Listener>();
const terminalListeners = new Map<string, Set<TerminalListener>>();
const chatListeners = new Map<string, Set<ChatListener>>();

function set(next: AppState) {
  if (next === state) return;
  state = next;
  // One failing subscriber must not stop the others (React's included).
  listeners.forEach((listener) => {
    try { listener(); } catch { reportFrontendDiagnostic("subscriberCrash"); }
  });
}

export const appStore = {
  get: () => state,
  subscribe(listener: Listener) {
    listeners.add(listener);
    return () => listeners.delete(listener);
  },
  error: () => connectError,
};

/** Select from the app state. The result is cached per state object, so a
 * selector may safely derive new arrays (e.g. `filter`) without looping. */
export function useApp<T>(select: (state: AppState) => T): T {
  const cache = useRef<{ state: AppState; select: (state: AppState) => T; value: T } | null>(null);
  return useSyncExternalStore(appStore.subscribe, () => {
    const hit = cache.current;
    if (hit && hit.state === state && hit.select === select) return hit.value;
    const value = select(state);
    // The same object: done. Comparing it structurally would serialize it
    // (the whole node catalog for `s.proxy`) on every render and event.
    if (hit && Object.is(hit.value, value)) {
      cache.current = { state, select, value };
      return value;
    }
    // Keep the previous value when it is structurally the same, so derived
    // arrays do not re-render consumers on unrelated events.
    if (hit && JSON.stringify(hit.value) === JSON.stringify(value)) {
      cache.current = { state, select, value: hit.value };
      return hit.value;
    }
    cache.current = { state, select, value };
    return value;
  });
}

/** The ordered output and lifecycle events for one terminal. */
export function onTerminalEvent(id: string, listener: TerminalListener): () => void {
  const set = terminalListeners.get(id) ?? new Set();
  set.add(listener);
  terminalListeners.set(id, set);
  return () => { set.delete(listener); if (!set.size) terminalListeners.delete(id); };
}

/** null asks a mounted chat to refresh after a guest reconnect. */
export function onChatEvent(id: string, listener: ChatListener): () => void {
  const set = chatListeners.get(id) ?? new Set();
  set.add(listener);
  chatListeners.set(id, set);
  return () => { set.delete(listener); if (!set.size) chatListeners.delete(id); };
}

let connecting: Promise<void> | null = null;
let loadingNetwork: Promise<void> | null = null;

/** Hydrate the Mac's network even when the guest has no snapshot yet. */
export function refreshNetworkStatus(): Promise<void> {
  if (loadingNetwork) return loadingNetwork;
  const before = state.network;
  const beforeCatalog = state.proxy;
  // An event or guest snapshot may already have supplied newer host state.
  const status = networkIpc.state().then((network) => {
    if (state.network === before) set({ ...state, network, networkLoaded: true, networkLoadError: null });
  }).catch((error: unknown) => {
    if (state.network === before && !state.networkLoaded) {
      set({ ...state, networkLoadError: toIpcError(error).message });
    }
  });
  const catalog = proxyIpc.catalog().then((proxy) => {
    if (state.proxy === beforeCatalog) set({ ...state, proxy });
  });
  loadingNetwork = Promise.allSettled([status, catalog]).then(() => {
    loadingNetwork = null;
  });
  return loadingNetwork;
}

/** Load the synced projects (owned by the Mac app, so not in a snapshot). */
export function refreshSyncs(): Promise<void> {
  return syncIpc.list().then((syncs) => set({ ...state, syncs })).catch(() => undefined);
}

/** Subscribe to backend events, then load the snapshot. Events that arrive
 * before the snapshot are replayed on top of it. */
export function connect(): Promise<void> {
  if (connecting) return connecting;
  const early: AppEvent[] = [];
  let loaded = false;
  connecting = appIpc
    .subscribe((event) => {
      if (
        event.type === "chatItem"
        || event.type === "chatPlan"
        || event.type === "chatSubagent"
        || event.type === "chatTask"
        || event.type === "chatState"
        || event.type === "chatModel"
        || event.type === "chatPermission"
        || event.type === "chatUsage"
        || event.type === "chatControls"
      ) {
        chatListeners.get(event.id)?.forEach((listener) => listener(event));
        if (event.type === "chatState") noteChatState(event.id, event.state.state);
        return;
      }
      if (event.type === "snapshot") chatListeners.forEach((set) => set.forEach((listener) => listener(null)));
      if (event.type === "sessionOutput" || event.type === "terminalStatus") {
        terminalListeners.get(event.id)?.forEach((listener) => listener(event));
        if (event.type === "sessionOutput") {
          if (state.sessions.some((s) => s.id === event.id && s.agent !== null)) noteOutput(event.id);
          return;
        }
      }
      if (!loaded) early.push(event);
      else set(applyEvent(state, event));
    })
    .then((snapshot: HostSnapshot) => {
      let next = fromHostSnapshot(state, snapshot);
      for (const event of early) next = applyEvent(next, event);
      loaded = true;
      connectError = null;
      set(next);
      startClockSync();
      startNetworkMonitor();
      void refreshNetworkStatus();
      void refreshSyncs();
    })
    .catch((error: unknown) => {
      connectError = error instanceof Error ? error.message : String(error);
      connecting = null;
      listeners.forEach((listener) => listener());
      throw error;
    });
  return connecting;
}

/** Local patches after a direct command result (the event will agree). */
export function patchState(update: (state: AppState) => AppState) {
  set(update(state));
}
