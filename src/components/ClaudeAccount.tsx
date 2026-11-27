import { Alert, Badge, Loader } from "@mantine/core";
import { useState } from "react";
import { currentClaudeAccount } from "../lib/agentsHub";
import { formatGuestDateTime } from "../lib/time";
import { useAutoRefresh } from "../lib/useAutoRefresh";
import { agentsIpc } from "../services/ipc/agents";
import { toIpcError } from "../services/ipc/common";
import type { AgentStatus, ClaudeAccount as Account, ClaudeUsage } from "../services/ipc/generated/core";
import { AccountLimit } from "./AccountLimits";

export function useClaudeUsage(account: Account | null | undefined, enabled: boolean) {
  const id = account?.method === "subscription" && !account.usageBased ? account.id ?? undefined : undefined;
  const [result, setResult] = useState<{ id: string; usage: ClaudeUsage | null; error: string | null } | null>(null);
  useAutoRefresh(async (isCurrent) => {
    if (!id) return;
    try {
      const usage = await agentsIpc.claudeUsage(id);
      if (isCurrent()) setResult({ id, usage, error: null });
    } catch (cause) {
      if (!isCurrent()) return;
      const problem = toIpcError(cause);
      const changed = problem.code === "conflict";
      setResult((last) => ({ id, usage: !changed && last?.id === id ? last.usage : null, error: problem.message }));
      if (changed) await agentsIpc.refresh().catch(() => undefined);
    }
  }, { enabled: enabled && !!id, refreshKey: id, minGap: 60_000, interval: 180_000 });
  const current = result?.id === id ? result : null;
  return { loading: enabled && !!id && !current, usage: current?.usage ?? null, error: current?.error ?? null };
}

export function ClaudeAccount({ status, hideLimits = false }: { status: AgentStatus; hideLimits?: boolean }) {
  const account = currentClaudeAccount(status);
  const unavailable = status.claudeLogin?.state === "unavailable";
  const { loading, usage, error } = useClaudeUsage(account, status.installed);
  const identity = [account?.email, account?.organization].filter(Boolean).join(" · ");

  return (
    <section className="min-w-0 grid gap-3">
      <div className="ag-account-banner">
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-2">
            <strong className="text-md font-semibold text-fg [overflow-wrap:anywhere]">{identity || (status.authenticated ? "Signed in to Claude Code" : "Not signed in")}</strong>
            {account?.plan && <Badge size="xs" variant="light">{account.plan}</Badge>}
          </div>
          {account && <p className="m-0 mt-0.5 text-xs text-muted">{account.provider ?? status.authDetail}</p>}
        </div>
      </div>
      {!account && !status.authenticated && !unavailable && <p className="agents-hint m-0">Use <code>/login</code> in Claude Code, or run <code>claude auth login</code> from a workspace shell.</p>}
      {!account && (unavailable || status.authenticated) && <p className="agents-hint m-0" role="status">{status.authDetail}</p>}
      {account?.usageBased && <p className="agents-hint m-0">This seat uses usage-based billing.</p>}
      {account?.method === "subscription" && !account.usageBased && !account.id && <p className="agents-hint m-0">Limits will appear when Claude Code reports the account identity.</p>}
      {account && account.method !== "subscription" && <p className="agents-hint m-0">Subscription limits are available for Claude.ai sign-ins.</p>}
      {error && <Alert color="yellow">{error}</Alert>}
      {loading && <p className="row agents-hint m-0" role="status"><Loader size={12} /> Reading account limits…</p>}
      {usage && <>
        {!hideLimits && usage.windows.length > 0 && <div className="account-limits-grid">{usage.windows.map((window) => <AccountLimit key={window.label} scope={`claude:${account?.id ?? "default"}`} label={window.label} used={window.usedPercent} resetsAt={window.resetsAt} resetText={window.resetText} />)}</div>}
        {usage.noLimits && usage.windows.length === 0 && <p className="agents-hint m-0">Claude Code reports no usage meters for this plan.</p>}
        <p className="agents-hint m-0">Last checked {formatGuestDateTime(usage.refreshedAt)} · Reported by Claude Code</p>
      </>}
    </section>
  );
}
