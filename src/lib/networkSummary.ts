// The Mac's network status, worded consistently across the sidebar and diagnostics.
import type { GeoInfo, NetworkCheckItem, NetworkCheckReport, NetworkStatus } from "../services/ipc/generated/core";
import type { AppState } from "../state/reducer";

export type NetTone = "ok" | "busy" | "warn" | "bad" | "off";
export type FixTarget = "region" | "route" | "browser";

export interface NetSummary {
  tone: NetTone;
  title: string;
  detail: string;
  /** The first required check that keeps the internet closed. */
  blocker: NetworkCheckItem | null;
}

/** Checks in dependency order: an earlier failure usually explains later ones. */
const ORDER = ["route", "gateway", "dns", "transport", "boundary", "exit", "timezone", "identity", "language", "browser", "browserLive"];

const rank = (id: string) => {
  const index = ORDER.indexOf(id);
  return index < 0 ? ORDER.length : index;
};

/** Short reasons for the first required check that failed. */
const REASON: Record<string, string> = {
  route: "a node in the route is unreachable",
  gateway: "the gateway is not connected",
  dns: "names do not resolve",
  transport: "connections through the route fail",
  boundary: "traffic isolation is not confirmed",
  exit: "the exit location is not confirmed",
  timezone: "the VM's time zone does not match the exit",
  identity: "the VM's hostname or account is unknown",
  language: "the VM's language is unknown",
  browser: "Chrome's installed settings do not match",
  browserLive: "Chrome runs on older settings",
};

export function sortChecks(checks: readonly NetworkCheckItem[]): NetworkCheckItem[] {
  return checks.slice().sort((a, b) => rank(a.id) - rank(b.id));
}

/** Required checks that have not passed, most fundamental first. */
export function blockingChecks(report: NetworkCheckReport | null): NetworkCheckItem[] {
  if (!report) return [];
  return sortChecks(report.checks.filter((check) => check.required && check.status !== "pass"));
}

/**
 * A report as the Network Check shows it: what blocks the internet first,
 * then optional checks worth a look, and everything that passed folded away.
 */
export function splitChecks(report: NetworkCheckReport): {
  problems: NetworkCheckItem[];
  notes: NetworkCheckItem[];
  passed: NetworkCheckItem[];
} {
  const sorted = sortChecks(report.checks);
  return {
    problems: sorted.filter((check) => check.required && check.status !== "pass"),
    notes: sorted.filter((check) => !check.required && (check.status === "fail" || check.status === "unknown")),
    passed: sorted.filter((check) => check.status === "pass" || (!check.required && check.status === "skipped")),
  };
}

export function blockingReason(check: NetworkCheckItem): string {
  return REASON[check.id] ?? `${check.title.toLowerCase()} did not pass`;
}

/** What the person can do about one check that did not pass. */
export function checkFix(check: NetworkCheckItem): { label: string; target: FixTarget } | null {
  if (check.status === "pass" || check.status === "skipped") return null;
  if (check.id === "timezone" || check.id === "language") {
    return { label: "Fix in Region", target: "region" };
  }
  // The browser's own settings and its live state are managed on the Chrome page.
  if (check.id === "browser" || check.id === "browserLive") {
    return { label: "Open browser environment", target: "browser" };
  }
  if (check.id === "route" || check.id === "exit") return { label: "Open route settings", target: "route" };
  return null;
}

const capital = (text: string) => text.charAt(0).toUpperCase() + text.slice(1);

const RECHECK_LIMIT = 3;

export function networkSummary(status: NetworkStatus | null, machineRunning: boolean, loadError?: string | null): NetSummary {
  const none = { blocker: null };
  if (!machineRunning) {
    return { tone: "off", title: "Offline", detail: "The machine is stopped", ...none };
  }
  if (!status) {
    return loadError
      ? { tone: "bad", title: "Status unavailable", detail: loadError, ...none }
      : { tone: "busy", title: "Loading network", detail: "Reading the machine’s network status", ...none };
  }
  if (!status.available) {
    return { tone: "off", title: "Unmanaged", detail: "This machine has no gateway", ...none };
  }
  if (status.paused) {
    return { tone: "warn", title: "Blocked", detail: "Internet access is paused", ...none };
  }
  if (status.exitPolicyError) {
    return { tone: "bad", title: "Blocked", detail: status.exitPolicyError, ...none };
  }
  if (status.connectivity === "rechecking") {
    const attempt = Math.min(status.failedChecks + 1, RECHECK_LIMIT);
    return { tone: "busy", title: "Rechecking", detail: `Attempt ${attempt} of ${RECHECK_LIMIT}`, ...none };
  }
  if (status.error || !status.running) {
    return { tone: "bad", title: "Blocked", detail: status.error ?? "The gateway is not running", ...none };
  }
  if (status.gate === "open" && status.connectivity === "normal") {
    const hops = status.route.chain.length;
    const detail = status.route.enabled ? `Through ${hops} node${hops === 1 ? "" : "s"}` : "Direct connection";
    return { tone: "ok", title: "Allowed", detail, ...none };
  }
  if (status.gate === "checking" && status.connectivity !== "blocked") {
    return { tone: "busy", title: "Checking", detail: "Verifying the route and the VM's environment", ...none };
  }
  const report = status.report?.revision === status.revision ? status.report : null;
  const blocker = blockingChecks(report)[0] ?? null;
  const reason = blocker ? capital(blockingReason(blocker)) : status.gateError ?? "Not verified yet";
  return {
    tone: "bad",
    title: "Blocked",
    detail: status.connectivity === "blocked" ? status.connectivityError ?? reason : reason,
    blocker,
  };
}

export function selectNetworkSummary(state: AppState): NetSummary {
  const summary = networkSummary(state.networkLoaded ? state.network : null, state.machine?.state === "running", state.networkLoadError);
  if (state.machine?.state === "notInstalled") return { ...summary, detail: "The machine is not installed" };
  return summary;
}

const IP_TYPES: Record<NonNullable<GeoInfo["ipType"]>, string> = {
  mobile: "Mobile network",
  residential: "Residential",
  commercial: "Business",
  server: "Data center",
  unknown: "Unknown type",
};

export function exitIpTypeLabel(type: GeoInfo["ipType"], inferred = false): string {
  return type === "residential" && inferred ? "Residential (estimated)" : IP_TYPES[type ?? "unknown"];
}
