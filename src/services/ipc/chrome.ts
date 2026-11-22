import { invokeIpc, invokeTask } from "./common";
import type { BrowserEnvironmentView, BrowserSettings } from "./generated/core";

export type { BrowserEnvironmentView, BrowserSettings };

export interface ChromeStatus {
  configured: boolean;
  installed: boolean | null;
  running: boolean | null;
  version: string | null;
  viewerReady: boolean | null;
  displayProtocols: DisplayProtocol[];
  canEnableSpice: boolean;
  install: ChromeInstallLog | null;
}

export interface ChromeInstallLog {
  running: boolean;
  logPath: string;
  lines: string[];
}

export type DisplayProtocol = "vnc" | "spice";
export interface ChromeDisplay { protocol: DisplayProtocol; url: string }

/** A graphical app installed in the VM's desktop. */
export interface DesktopApp { id: string; name: string; comment: string | null }
export interface NativeDisplayStatus {
  enabled: boolean;
  recoveryRequired: boolean;
  zoom: number | null;
  width: number;
  height: number;
  scale: number;
  logicalWidth: number;
  logicalHeight: number;
  chromeWayland: boolean;
}

export type NativeDisplayRequest =
  | { action: "status" | "restore" }
  | { action: "apply"; width: number; height: number; scale: number; zoom: number };

export const chromeIpc = {
  status: () => invokeIpc<ChromeStatus>("chrome_status"),
  install: () => invokeTask<void>("chrome_install"),
  open: (url = "about:blank") => invokeIpc<void>("chrome_open", { url }),
  stop: () => invokeIpc<void>("chrome_stop"),
  paste: (text: string) => invokeIpc<void>("chrome_paste", { text }),
  nativeDisplay: (request: NativeDisplayRequest) => invokeIpc<NativeDisplayStatus>("chrome_native_display", { request }),
  display: (protocol: DisplayProtocol) => invokeIpc<ChromeDisplay>("chrome_display", {
    protocol,
    parentOrigin: protocol === "spice" ? window.location.origin : undefined,
  }),
  enableSpice: () => invokeTask<void>("chrome_enable_spice"),
};

/** The managed browser's environment (its panel on the Chrome page). */
export const browserIpc = {
  /** `check` also audits the installed profile and the live Chrome. */
  environment: (check: boolean) => invokeIpc<BrowserEnvironmentView>("browser_environment", { check }),
  /** Applies live for a region change; otherwise Chrome restarts. */
  save: (settings: BrowserSettings) => invokeIpc<BrowserEnvironmentView>("browser_settings_save", { settings }),
};

export const desktopIpc = {
  apps: () => invokeIpc<DesktopApp[]>("desktop_apps"),
  /** Starts the app on its own workspace; switch with the bar at the top. */
  launch: (id: string) => invokeIpc<void>("desktop_launch", { id }),
};
