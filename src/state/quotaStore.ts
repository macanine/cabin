/**
 * Unified, deduplicated quota & account-limit store across all five agents.
 *
 * Eliminates duplicate IPC fetches between AgentsView, ClaudeAccount,
 * CodexAccountsPanel, and UsageView, and normalizes every agent's 5h / 7d
 * rolling windows, reset times, credits, reset cards, and subscription expiry
 * into one consistent model.
 */
import { useEffect, useSyncExternalStore } from "react";
import { agentsIpc } from "../services/ipc/agents";
import { toIpcError } from "../services/ipc/common";
import type {
  AgentKind,
  AgentStatus,
  ClaudeAccount,
  ClaudeUsage,
  CodexAccounts,
  CodexCredits,
  CodexQuota,
  CodexResetCard,
} from "../services/ipc/generated/core";
import { currentClaudeAccount } from "../lib/agentsHub";
import {
  classifyWindowTier,
  shortLimitLabel,
  type LimitWindow,
  type WindowTier,
} from "../lib/agentLimits";
import { appStore, useApp } from "./store";

export interface UnifiedQuotaWindow extends LimitWindow {
  id: string;
  agent: AgentKind;
  scope: string;
  accountLabel: string | null;
  plan: string | null;
  shortLabel: string;
  tier: WindowTier;
  leftPercent: number;
}

export interface AgentQuotaSummary {
  agent: AgentKind;
  /** Whether this agent currently exposes live rolling 5h/7d plan windows. */
  hasLiveWindows: boolean;
  accountLabel: string | null;
  plan: string | null;
  /** Parsed Unix ms when a paid subscription expires, if reported (e.g. Codex). */
  subscriptionUntil: number | null;
  subscriptionUntilRaw: string | null;
  windows: UnifiedQuotaWindow[];
  credits: CodexCredits | null;
  resetCards: CodexResetCard[];
  resetCreditCount: number;
  restriction: string | null;
  noLimits: boolean;
  refreshedAt: number | null;
  loading: boolean;
  error: string | null;
}

interface QuotaStoreState {
  claudeAccountId: string | null;
  claudeUsage: ClaudeUsage | null;
  claudeLoading: boolean;
  claudeError: string | null;
  claudeFetchedAt: number;

  codexData: CodexAccounts | null;
  codexLoading: boolean;
  codexError: string | null;
  codexFetchedAt: number;
}

const TTL_MS = 60_000;

let store: QuotaStoreState = {
  claudeAccountId: null,
  claudeUsage: null,
  claudeLoading: false,
  claudeError: null,
  claudeFetchedAt: 0,

  codexData: null,
  codexLoading: false,
  codexError: null,
  codexFetchedAt: 0,
};

const listeners = new Set<() => void>();
let claudeInFlight: Promise<void> | null = null;
let codexInFlight: Promise<void> | null = null;

function emit(patch: Partial<QuotaStoreState>) {
  store = { ...store, ...patch };
  listeners.forEach((listener) => listener());
}

function parseSubscriptionMs(value: string | null | undefined): number | null {
  if (!value) return null;
  const parsed = Date.parse(value);
  return Number.isNaN(parsed) ? null : parsed;
}

function claudeSubscriptionId(agents: readonly AgentStatus[]): { account: ClaudeAccount | null; id: string | null } {
  const status = agents.find((agent) => agent.kind === "claude" && agent.installed);
  const account = currentClaudeAccount(status);
  const id = account?.method === "subscription" && !account.usageBased ? (account.id ?? null) : null;
  return { account, id };
}

function codexIsInstalled(agents: readonly AgentStatus[]): boolean {
  return agents.some((agent) => agent.kind === "codex" && agent.installed);
}

/** Refresh Claude subscription /usage (deduplicated). */
export async function refreshClaudeQuota(force = false): Promise<void> {
  const state = appStore.get();
  if (!state.guestReady) return;
  const { id } = claudeSubscriptionId(state.agents);
  if (!id) {
    if (store.claudeAccountId !== null || store.claudeUsage !== null) {
      emit({ claudeAccountId: null, claudeUsage: null, claudeLoading: false, claudeError: null });
    }
    return;
  }
  if (claudeInFlight) return claudeInFlight;
  const fresh = !force && store.claudeAccountId === id && store.claudeUsage != null && Date.now() - store.claudeFetchedAt < TTL_MS;
  if (fresh) return;

  emit({
    claudeAccountId: id,
    claudeLoading: true,
    ...(store.claudeAccountId !== id ? { claudeUsage: null, claudeError: null } : {}),
  });

  claudeInFlight = agentsIpc
    .claudeUsage(id)
    .then((usage) => {
      emit({
        claudeAccountId: id,
        claudeUsage: usage,
        claudeLoading: false,
        claudeError: null,
        claudeFetchedAt: Date.now(),
      });
    })
    .catch(async (cause: unknown) => {
      const problem = toIpcError(cause);
      const changed = problem.code === "conflict";
      emit({
        claudeAccountId: id,
        claudeUsage: !changed && store.claudeAccountId === id ? store.claudeUsage : null,
        claudeLoading: false,
        claudeError: problem.message,
      });
      if (changed) await agentsIpc.refresh().catch(() => undefined);
    })
    .finally(() => {
      claudeInFlight = null;
    });
  return claudeInFlight;
}

/** Refresh Codex saved accounts & live app-server quota (deduplicated). */
export async function refreshCodexQuota(force = false): Promise<void> {
  const state = appStore.get();
  if (!state.guestReady || !codexIsInstalled(state.agents)) return;
  if (codexInFlight) return codexInFlight;
  const fresh = !force && store.codexData != null && Date.now() - store.codexFetchedAt < TTL_MS;
  if (fresh) return;

  emit({ codexLoading: true });
  codexInFlight = agentsIpc
    .codexAccounts()
    .then(async (accounts) => {
      emit({ codexData: accounts, codexError: null, codexFetchedAt: Date.now() });
      if (accounts.activeAccountId) {
        const refreshed = await agentsIpc.refreshCodexQuota();
        emit({ codexData: refreshed, codexError: null, codexFetchedAt: Date.now() });
      }
    })
    .catch((cause: unknown) => {
      emit({ codexError: toIpcError(cause).message });
    })
    .finally(() => {
      codexInFlight = null;
      emit({ codexLoading: false });
    });
  return codexInFlight;
}

/** Push mutated Codex account/quota state directly into the shared store. */
export function setCodexAccountsSnapshot(data: CodexAccounts) {
  emit({ codexData: data, codexError: null, codexFetchedAt: Date.now() });
}

export function setCodexQuotaSnapshot(quota: CodexQuota | null) {
  if (!store.codexData) return;
  emit({ codexData: { ...store.codexData, quota }, codexFetchedAt: Date.now() });
}

/** Refresh every agent's live quota windows in parallel. */
export async function refreshAllQuotas(force = false): Promise<void> {
  await Promise.allSettled([refreshClaudeQuota(force), refreshCodexQuota(force)]);
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => { listeners.delete(listener); };
}

function buildClaudeSummary(agents: readonly AgentStatus[], state: QuotaStoreState): AgentQuotaSummary {
  const { account, id } = claudeSubscriptionId(agents);
  const usage = id && state.claudeAccountId === id ? state.claudeUsage : null;
  const scope = `claude:${id ?? "default"}`;
  const accountLabel = account?.email ?? account?.organization ?? null;
  const windows: UnifiedQuotaWindow[] = (usage?.windows ?? []).map((w) => {
    const used = Math.max(0, Math.min(100, w.usedPercent));
    const shortLabel = shortLimitLabel(w.label);
    return {
      id: `${scope}:${w.label}`,
      agent: "claude",
      scope,
      accountLabel,
      plan: account?.plan ?? null,
      label: w.label,
      shortLabel,
      tier: classifyWindowTier(w),
      usedPercent: used,
      leftPercent: Math.max(0, 100 - used),
      windowMinutes: null,
      resetsAt: w.resetsAt,
      resetText: w.resetText,
    };
  });

  return {
    agent: "claude",
    hasLiveWindows: windows.length > 0,
    accountLabel,
    plan: account?.plan ?? null,
    subscriptionUntil: null,
    subscriptionUntilRaw: null,
    windows,
    credits: null,
    resetCards: [],
    resetCreditCount: 0,
    restriction: null,
    noLimits: Boolean(usage?.noLimits),
    refreshedAt: usage?.refreshedAt ?? null,
    loading: Boolean(id && state.claudeLoading && !usage),
    error: id && state.claudeAccountId === id ? state.claudeError : null,
  };
}

function formatPlanName(plan: string | null | undefined): string | null {
  if (!plan) return null;
  return plan.replace(/_/g, " ").replace(/\b\w/g, (letter) => letter.toUpperCase());
}

function buildCodexSummary(agents: readonly AgentStatus[], state: QuotaStoreState): AgentQuotaSummary {
  const installed = codexIsInstalled(agents);
  const data = installed ? state.codexData : null;
  const active = data?.accounts.find((account) => account.active) ?? null;
  const quota = data?.quota ?? null;
  const scope = `codex:${data?.activeAccountId ?? "default"}`;
  const plan = formatPlanName(quota?.planType ?? active?.planType);
  const accountLabel = active?.email ?? (active?.authMode === "apikey" ? "API key" : null);

  const windows: UnifiedQuotaWindow[] = (quota && !quota.error ? quota.windows : []).map((w) => {
    const used = Math.max(0, Math.min(100, w.usedPercent));
    const shortLabel = shortLimitLabel(w.label);
    return {
      id: `${scope}:${w.kind}:${w.windowMinutes ?? w.label}`,
      agent: "codex",
      scope,
      accountLabel,
      plan,
      label: w.label,
      shortLabel,
      tier: classifyWindowTier(w),
      usedPercent: used,
      leftPercent: Math.max(0, 100 - used),
      windowMinutes: w.windowMinutes,
      resetsAt: w.resetsAt,
      resetText: null,
    };
  });

  let restriction: string | null = null;
  if (quota?.ordinaryUsageAllowed === false) {
    restriction = "Included-usage limit reached";
  } else if (quota?.spendControlReached || quota?.rateLimitReachedType) {
    restriction = quota.rateLimitReachedType
      ? `Rate limit: ${quota.rateLimitReachedType.replace(/_/g, " ")}`
      : "Spend control or rate limit active";
  }

  return {
    agent: "codex",
    hasLiveWindows: windows.length > 0,
    accountLabel,
    plan,
    subscriptionUntil: parseSubscriptionMs(active?.subscriptionUntil),
    subscriptionUntilRaw: active?.subscriptionUntil ?? null,
    windows,
    credits: quota?.credits ?? null,
    resetCards: quota?.resetCredits.filter((card) => card.status === "available") ?? [],
    resetCreditCount: quota?.resetCreditCount ?? 0,
    restriction,
    noLimits: Boolean(quota && !quota.error && quota.windows.length === 0),
    refreshedAt: quota?.refreshedAt ?? null,
    loading: installed && state.codexLoading && !quota,
    error: quota?.error ?? state.codexError,
  };
}

function buildStaticSummary(agent: AgentKind): AgentQuotaSummary {
  return {
    agent,
    hasLiveWindows: false,
    accountLabel: null,
    plan: null,
    subscriptionUntil: null,
    subscriptionUntilRaw: null,
    windows: [],
    credits: null,
    resetCards: [],
    resetCreditCount: 0,
    restriction: null,
    noLimits: false,
    refreshedAt: null,
    loading: false,
    error: null,
  };
}

/**
 * Subscribe to the unified quota state across all agents and keep it fresh
 * while mounted.
 */
export function useUnifiedQuotas(enabled = true) {
  const guestReady = useApp((state) => state.guestReady);
  const agents = useApp((state) => state.agents);
  const snap = useSyncExternalStore(subscribe, () => store);

  const claudeId = claudeSubscriptionId(agents).id;
  const codexReady = codexIsInstalled(agents);

  useEffect(() => {
    if (!enabled || !guestReady) return;
    void refreshAllQuotas(false);
    const timer = window.setInterval(() => {
      if (document.visibilityState !== "hidden") void refreshAllQuotas(false);
    }, 180_000);
    const onVisible = () => {
      if (document.visibilityState === "visible") void refreshAllQuotas(false);
    };
    document.addEventListener("visibilitychange", onVisible);
    return () => {
      clearInterval(timer);
      document.removeEventListener("visibilitychange", onVisible);
    };
  }, [enabled, guestReady, claudeId, codexReady]);

  const byAgent: Record<AgentKind, AgentQuotaSummary> = {
    claude: buildClaudeSummary(agents, snap),
    codex: buildCodexSummary(agents, snap),
    opencode: buildStaticSummary("opencode"),
    pi: buildStaticSummary("pi"),
    antigravity: buildStaticSummary("antigravity"),
  };

  const allWindows: UnifiedQuotaWindow[] = [
    ...byAgent.claude.windows,
    ...byAgent.codex.windows,
    ...byAgent.opencode.windows,
    ...byAgent.pi.windows,
    ...byAgent.antigravity.windows,
  ];

  const anyLoading = snap.claudeLoading || snap.codexLoading;
  const refreshedAt = Math.max(
    byAgent.claude.refreshedAt ?? 0,
    byAgent.codex.refreshedAt ?? 0,
  ) || null;

  return {
    byAgent,
    allWindows,
    codexData: snap.codexData,
    codexError: snap.codexError,
    codexLoading: snap.codexLoading,
    anyLoading,
    refreshedAt,
    refreshAll: (force = true) => refreshAllQuotas(force),
    refreshAgent: (kind: AgentKind, force = true) =>
      kind === "claude" ? refreshClaudeQuota(force) : kind === "codex" ? refreshCodexQuota(force) : Promise.resolve(),
  };
}
