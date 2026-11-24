import { useSyncExternalStore } from "react";
import type { AgentKind } from "../services/ipc/generated/core";
import type { TaskListMode } from "../lib/taskList";
import type { AgentSection } from "../lib/agentsHub";

/** Settings tabs, in display order (views/SettingsView.tsx). */
export const SETTINGS_TABS = ["general", "network", "tools", "machine", "storage", "permissions"] as const;
export type SettingsTab = (typeof SETTINGS_TABS)[number];

export type View =
  | { kind: "new"; workspaceId?: string }
  | { kind: "session"; id: string }
  | { kind: "install" }
  | { kind: "chrome" }
  /** Agents register and Providers, with optional agent selection. */
  | { kind: "agents"; agent?: AgentKind; section?: AgentSection }
  /** Token usage across agents (views/UsageView.tsx). */
  | { kind: "usage" }
  | { kind: "mcp"; agent?: AgentKind }
  | { kind: "skills"; agent?: AgentKind }
  | { kind: "settings"; tab?: SettingsTab };

/** The session panel beside a chat or terminal (components/workbench). */
export const PANEL_TABS = ["files", "changes", "browser"] as const;
export type PanelTab = (typeof PANEL_TABS)[number];
export interface PanelState { open: boolean; tab: PanelTab; width: number }
export const PANEL_MIN = 300;
export const PANEL_MAX = 1100;

export interface Toast {
  id: number;
  /** `loading` stays until it is updated (see `updateToast`). */
  tone: "info" | "success" | "error" | "loading";
  message: string;
  /** A second, quieter line. */
  detail?: string;
  /** A small preview (an object URL; revoked when the toast goes). */
  image?: string;
  /** How long it stays, ms; 0 stays until updated or dismissed. */
  duration: number;
  /** Bumped on every update so the new message is announced. */
  revision: number;
  /** Set for the short exit animation before it is removed. */
  leaving?: boolean;
}

export interface ToastOptions {
  detail?: string;
  image?: string;
  duration?: number;
}

/** The exit animation's length (components/Toasts.tsx, controls.css). */
const TOAST_EXIT_MS = 260;
/** Toasts kept at once; older ones make room. */
const TOAST_LIMIT = 4;

const defaultDuration = (tone: Toast["tone"]) => (tone === "loading" ? 0 : tone === "error" ? 7000 : 3600);

/** Each toast's timer, so hovering the stack can pause them all. */
const toastTimers = new Map<number, { handle?: ReturnType<typeof setTimeout>; remaining: number; endsAt: number }>();

function scheduleToast(id: number, ms: number) {
  const current = toastTimers.get(id);
  if (current?.handle) clearTimeout(current.handle);
  if (ms <= 0) {
    toastTimers.delete(id);
    return;
  }
  const paused = ui.toastsPaused;
  toastTimers.set(id, {
    handle: paused ? undefined : setTimeout(() => uiActions.dismissToast(id), ms),
    remaining: ms,
    endsAt: Date.now() + ms,
  });
}

interface UiState {
  view: View;
  /** Visited views for Back/Forward; `historyIndex` points at `view`. */
  history: View[];
  historyIndex: number;
  taskListMode: TaskListMode;
  sidebarCollapsed: boolean;
  operationId: string | null;
  operationsOpen: boolean;
  networkCheckOpen: boolean;
  networkSettingsFocus: "route" | "region" | null;
  /** The browser environment panel on the Chrome page. */
  browserPanelOpen: boolean;
  panel: PanelState;
  toasts: Toast[];
  /** While the pointer is over the toasts, their timers wait. */
  toastsPaused: boolean;
}

const PANEL_KEY = "cabin.panel";

function readPanel(): PanelState {
  const fallback: PanelState = { open: false, tab: "changes", width: 440 };
  try {
    const saved = JSON.parse(localStorage.getItem(PANEL_KEY) ?? "null") as Partial<PanelState> | null;
    if (!saved) return fallback;
    return {
      open: saved.open === true,
      tab: PANEL_TABS.includes(saved.tab as PanelTab) ? (saved.tab as PanelTab) : fallback.tab,
      width: typeof saved.width === "number" ? Math.min(PANEL_MAX, Math.max(PANEL_MIN, saved.width)) : fallback.width,
    };
  } catch {
    return fallback;
  }
}

function savePanel(panel: PanelState) {
  try {
    localStorage.setItem(PANEL_KEY, JSON.stringify(panel));
  } catch {
    // Not remembered in this viewer.
  }
}

const COLLAPSED_KEY = "cabin.sidebarCollapsed";

function readCollapsed(): boolean {
  try {
    return localStorage.getItem(COLLAPSED_KEY) === "1";
  } catch {
    return false;
  }
}

const MODE_KEY = "cabin.taskListMode";
const HISTORY_LIMIT = 50;

function readMode(): TaskListMode {
  try {
    const value = localStorage.getItem(MODE_KEY);
    if (value === "project" || value === "session" || value === "recent" || value === "unread") return value;
  } catch {
    // Default below.
  }
  return "project";
}

let ui: UiState = {
  view: { kind: "new" },
  history: [{ kind: "new" }],
  historyIndex: 0,
  taskListMode: readMode(),
  sidebarCollapsed: readCollapsed(),
  operationId: null,
  operationsOpen: false,
  networkCheckOpen: false,
  networkSettingsFocus: null,
  browserPanelOpen: false,
  panel: readPanel(),
  toasts: [],
  toastsPaused: false,
};
const listeners = new Set<() => void>();
let toastId = 0;

function set(next: Partial<UiState>) {
  ui = { ...ui, ...next };
  listeners.forEach((listener) => listener());
}

export function useUi<T>(select: (state: UiState) => T): T {
  return useSyncExternalStore(
    (listener) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    () => select(ui),
  );
}

export const uiActions = {
  navigate(view: View) {
    if (sameView(view, ui.view)) return;
    const history = [...ui.history.slice(0, ui.historyIndex + 1), view].slice(-HISTORY_LIMIT);
    set({ view, history, historyIndex: history.length - 1 });
  },
  back() {
    if (ui.historyIndex > 0) set({ historyIndex: ui.historyIndex - 1, view: ui.history[ui.historyIndex - 1] });
  },
  forward() {
    if (ui.historyIndex < ui.history.length - 1) {
      set({ historyIndex: ui.historyIndex + 1, view: ui.history[ui.historyIndex + 1] });
    }
  },
  /** Drop history entries that point at a deleted session. */
  forgetSession(id: string) {
    const keep = (v: View) => !(v.kind === "session" && v.id === id);
    const history = ui.history.filter(keep);
    const view = keep(ui.view) ? ui.view : { kind: "new" as const };
    const fixed = history.length > 0 ? history : [view];
    set({ view, history: fixed, historyIndex: Math.max(0, fixed.findIndex((v) => sameView(v, view))) });
  },
  setTaskListMode(taskListMode: TaskListMode) {
    try {
      localStorage.setItem(MODE_KEY, taskListMode);
    } catch {
      // Not remembered in this viewer.
    }
    set({ taskListMode });
  },
  toggleSidebar() {
    const sidebarCollapsed = !ui.sidebarCollapsed;
    try {
      localStorage.setItem(COLLAPSED_KEY, sidebarCollapsed ? "1" : "0");
    } catch {
      // Not persisted in this viewer; the toggle still works.
    }
    set({ sidebarCollapsed });
  },
  /** Show a panel tab; the active tab's button closes the panel again. */
  togglePanel(tab?: PanelTab) {
    const current = ui.panel;
    const panel = tab == null
      ? { ...current, open: !current.open }
      : current.open && current.tab === tab ? { ...current, open: false } : { ...current, open: true, tab };
    savePanel(panel);
    set({ panel });
  },
  openPanel(tab: PanelTab) {
    const panel = { ...ui.panel, open: true, tab };
    savePanel(panel);
    set({ panel });
  },
  resizePanel(width: number, persist = false) {
    const panel = { ...ui.panel, width: Math.round(Math.min(PANEL_MAX, Math.max(PANEL_MIN, width))) };
    if (persist) savePanel(panel);
    set({ panel });
  },
  openOperation: (operationId: string | null) => set({ operationId }),
  showOperations: (operationsOpen: boolean) => set({ operationsOpen }),
  openNetworkCheck: () => set({ networkCheckOpen: true }),
  closeNetworkCheck: () => set({ networkCheckOpen: false }),
  focusNetworkSettings(target: "route" | "region") {
    uiActions.navigate({ kind: "settings", tab: "network" });
    set({ networkSettingsFocus: target });
  },
  clearNetworkSettingsFocus: () => set({ networkSettingsFocus: null }),
  /** Show the Chrome page with its browser environment panel open. */
  openBrowserEnvironment() {
    uiActions.navigate({ kind: "chrome" });
    set({ browserPanelOpen: true });
  },
  setBrowserPanel: (open: boolean) => set({ browserPanelOpen: open }),
  /** Shows a toast; returns its id for `updateToast` / `dismissToast`. */
  toast(message: string, tone: Toast["tone"] = "info", options: ToastOptions = {}): number {
    const id = ++toastId;
    const duration = options.duration ?? defaultDuration(tone);
    const live = ui.toasts.filter((toast) => !toast.leaving);
    for (const old of live.slice(0, Math.max(0, live.length - (TOAST_LIMIT - 1)))) uiActions.dismissToast(old.id);
    set({ toasts: [...ui.toasts, { id, tone, message, detail: options.detail, image: options.image, duration, revision: 0 }] });
    scheduleToast(id, duration);
    return id;
  },
  /** Changes a toast in place (e.g. loading → done); its timer starts over. */
  updateToast(id: number, patch: { message?: string; tone?: Toast["tone"]; detail?: string; duration?: number }) {
    const current = ui.toasts.find((toast) => toast.id === id && !toast.leaving);
    if (!current) return;
    const tone = patch.tone ?? current.tone;
    const duration = patch.duration ?? (patch.tone ? defaultDuration(tone) : current.duration);
    set({ toasts: ui.toasts.map((toast) => (toast.id === id ? { ...toast, ...patch, tone, duration, revision: toast.revision + 1 } : toast)) });
    scheduleToast(id, duration);
  },
  /** Plays the exit, then removes the toast. */
  dismissToast(id: number) {
    const toast = ui.toasts.find((item) => item.id === id && !item.leaving);
    if (!toast) return;
    scheduleToast(id, 0);
    const remove = () => {
      if (toast.image?.startsWith("blob:")) URL.revokeObjectURL(toast.image);
      set({ toasts: ui.toasts.filter((item) => item.id !== id) });
    };
    if (document.documentElement.dataset.perf === "lite" || window.matchMedia("(prefers-reduced-motion: reduce)").matches) {
      remove();
      return;
    }
    set({ toasts: ui.toasts.map((item) => (item.id === id ? { ...item, leaving: true } : item)) });
    setTimeout(remove, TOAST_EXIT_MS);
  },
  /** Hovering the toasts holds their timers; leaving resumes them. */
  pauseToasts(paused: boolean) {
    if (ui.toastsPaused === paused) return;
    const now = Date.now();
    for (const [id, timer] of toastTimers) {
      if (paused) {
        if (timer.handle) clearTimeout(timer.handle);
        toastTimers.set(id, { remaining: Math.max(0, timer.endsAt - now), endsAt: timer.endsAt });
      } else {
        toastTimers.set(id, {
          handle: setTimeout(() => uiActions.dismissToast(id), Math.max(400, timer.remaining)),
          remaining: timer.remaining,
          endsAt: now + Math.max(400, timer.remaining),
        });
      }
    }
    set({ toastsPaused: paused });
  },
  getView: () => ui.view,
};

export function sameView(a: View, b: View): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}

const LAST_KEY = "cabin.lastStart";

/** The workspace and agent used for the last new session (a convenience). */
export function lastStart(): { workspaceId?: string; agent?: AgentKind } {
  try {
    return JSON.parse(localStorage.getItem(LAST_KEY) ?? "{}") as { workspaceId?: string; agent?: AgentKind };
  } catch {
    return {};
  }
}

export function rememberStart(workspaceId: string, agent: AgentKind) {
  try {
    localStorage.setItem(LAST_KEY, JSON.stringify({ workspaceId, agent }));
  } catch {
    // Not remembered in this viewer; harmless.
  }
}
