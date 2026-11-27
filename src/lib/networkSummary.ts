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

export interface NetGuidance {
  headline: string;
  explanation: string;
  tip?: string;
}

/** Plain-language explanation of the current network gate state so blocked users know why and what to do. */
export function networkGuidance(status: NetworkStatus | null, summary: NetSummary): NetGuidance {
  if (!status || !status.available) {
    return {
      headline: "No managed gateway",
      explanation: "This machine doesn’t have a managed network gateway configured.",
    };
  }
  if (summary.tone === "busy") {
    return {
      headline: "Checking the connection…",
      explanation: "Verifying the outbound route, exit location, and VM environment before opening internet access.",
    };
  }
  if (status.paused) {
    return {
      headline: "Internet access is paused",
      explanation: "You paused outbound traffic for the VM. Agents and Chrome inside Ubuntu cannot reach the web until you resume.",
    };
  }
  if (summary.tone === "ok") {
    return {
      headline: "Internet access verified",
      explanation: "The VM’s route, public exit, and environment passed verification. Outbound traffic is open.",
    };
  }
  if (status.exitPolicyError) {
    return {
      headline: "Internet blocked: public IP changed",
      explanation: "Cabin locked the VM’s internet because your public exit IP changed while Static IP protection is on. This prevents agents and Chrome from silently appearing from an unexpected location.",
      tip: "Switched Wi-Fi, VPN, or proxy node on purpose? Approve the new exit. If your IP rotates often, switch Exit IP protection to Dynamic residential IP.",
    };
  }
  if (status.error || !status.running) {
    return {
      headline: "Network gateway isn’t running",
      explanation: "The local gateway process that bridges VM traffic to the internet stopped unexpectedly.",
      tip: "Try checking again, or restart the machine in Machine settings if the gateway does not recover.",
    };
  }
  const id = summary.blocker?.id;
  if (id === "timezone" || id === "language") {
    return {
      headline: "Internet blocked: VM region mismatch",
      explanation: "Ubuntu’s time zone or language doesn’t match your current exit location. Align it in VM region settings (or turn on Follow exit), then check again.",
    };
  }
  if (id === "browser" || id === "browserLive") {
    return {
      headline: "Internet blocked: Chrome environment out of sync",
      explanation: "Chrome’s configured or running profile doesn’t match the VM’s current exit region. Sync or restart Chrome in Browser environment to reopen internet access.",
    };
  }
  if (id === "route" || id === "dns" || id === "transport") {
    return {
      headline: "Internet blocked: connection failed",
      explanation: "The VM couldn’t reach the internet through the current route. Check your host network, VPN, or proxy nodes, then run a check again.",
    };
  }
  if (id === "exit") {
    return {
      headline: "Internet blocked: exit couldn’t be verified",
      explanation: summary.blocker?.title === "Public IP addresses differ"
        ? "The VM’s exit IP didn’t match the host verification check. Review your VPN or proxy route, then check again."
        : "Cabin couldn’t confirm the VM’s public exit location. Review your route or check again in a moment.",
    };
  }
  return {
    headline: summary.blocker ? `Internet blocked: ${blockingReason(summary.blocker)}` : "Internet access couldn’t be verified",
    explanation: "Outbound VM traffic stays locked until the network check passes.",
    tip: "Open diagnostics below to see which check failed, then run Check again.",
  };
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
