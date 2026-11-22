// The browser environment panel (components/BrowserEnvironmentPanel.tsx):
// what to say about the managed browser, and what a settings change costs.
import type { BrowserEnvironmentView, BrowserSettings } from "../services/ipc/generated/core";

export type EnvTone = "ok" | "warn" | "bad" | "muted" | "busy";
export type EnvAction = "restart" | "check" | null;

export interface EnvSummary {
  tone: EnvTone;
  title: string;
  detail: string;
  /** The one thing worth doing now, if any. */
  action: EnvAction;
}

/**
 * One line about the browser. Wrong installed settings block the machine's
 * internet, and so does an open Chrome on other values (fixed by restarting
 * it). A closed or sleeping Chrome is not checked and never blocks.
 */
export function environmentSummary(view: BrowserEnvironmentView | null, checking = false): EnvSummary {
  if (!view || (checking && !view.probe)) {
    return { tone: "busy", title: "Checking the browser", detail: "Reading its installed settings and open pages", action: null };
  }
  if (!view.enabled) {
    return { tone: "muted", title: "No managed browser", detail: "Set up Chrome to manage its environment", action: null };
  }
  if (!view.machineRunning) {
    return { tone: "muted", title: "Machine is not running", detail: "Start it to check or change the browser", action: null };
  }
  const probe = view.probe;
  if (!probe) {
    return { tone: "muted", title: "Not checked yet", detail: "Check to compare the browser with its settings", action: "check" };
  }
  if (probe.error) {
    return { tone: "bad", title: "Settings are not applied", detail: probe.error, action: "check" };
  }
  switch (probe.live) {
    case "observed":
      return view.liveMatches === false
        ? { tone: "bad", title: "Chrome runs on older settings", detail: "The machine's internet is blocked until Chrome restarts with its settings", action: "restart" }
        : { tone: "ok", title: "Chrome matches its settings", detail: "Installed settings and open pages agree", action: null };
    case "notRunning":
      return { tone: "ok", title: "Settings installed", detail: "Chrome is not running; it starts with these settings", action: null };
    case "idle":
      return {
        tone: "ok",
        title: "Settings installed",
        detail: `${probe.liveDetail ?? "No page to inspect"}. Pages are checked when they are open and awake.`,
        action: null,
      };
    case "unavailable":
      return { tone: "warn", title: "Live pages not checked", detail: probe.liveDetail ?? "The open pages could not be inspected", action: "check" };
  }
}

/** Region fields: applied live, Chrome keeps running. */
const LIVE_FIELDS: (keyof BrowserSettings)[] = ["timezone", "locale"];

export function settingsChanged(before: BrowserSettings, after: BrowserSettings): boolean {
  return (Object.keys(after) as (keyof BrowserSettings)[]).some((key) => before[key] !== after[key]);
}

/**
 * Whether saving `after` restarts Chrome: anything beyond the time zone and
 * language rewrites the profile (and turning "follow the machine" on or off
 * can change the platform and hardware hints too).
 */
export function restartsChrome(before: BrowserSettings, after: BrowserSettings): boolean {
  return (Object.keys(after) as (keyof BrowserSettings)[])
    .filter((key) => !LIVE_FIELDS.includes(key))
    .some((key) => before[key] !== after[key]);
}

/** Device memory buckets the browser can report (navigator.deviceMemory). */
export const MEMORY_BUCKETS = [0.25, 0.5, 1, 2, 4, 8] as const;

/** Common platform strings; any other value can be typed. */
export const PLATFORMS = ["Linux aarch64", "Linux x86_64", "Win32", "MacIntel"] as const;

/** A short problem with the draft, or null when it can be saved. */
export function settingsProblem(draft: BrowserSettings): string | null {
  const agent = draft.userAgent.trim();
  if (!agent) return "Enter a user agent";
  if (agent.length > 512 || /[\r\n\t]/.test(agent)) return "The user agent must be one line of up to 512 characters";
  if (draft.followMachine) return null;
  if (!/^[A-Za-z0-9/_+-]+$/.test(draft.timezone.trim())) return "Choose a time zone such as Europe/Berlin";
  if (!/^[A-Za-z0-9_.@-]+$/.test(draft.locale.trim())) return "Choose a language such as en-US";
  const platform = draft.platform.trim();
  if (!platform || platform.length > 64) return "Enter a platform such as Win32";
  if (!Number.isInteger(draft.hardwareConcurrency) || draft.hardwareConcurrency < 1 || draft.hardwareConcurrency > 128) {
    return "CPU cores must be between 1 and 128";
  }
  if (!(MEMORY_BUCKETS as readonly number[]).includes(draft.deviceMemory)) return "Choose a device memory size";
  return null;
}
