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
    <section className="min-w-0 grid gap-2.5">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="min-w-0">
          <strong className="text-sm font-medium text-fg [overflow-wrap:anywhere]">
            {identity || (status.authenticated ? "Signed in to Claude Code" : "Not signed in")}
          </strong>
          {account?.usageBased && <p className="m-0 mt-0.5 text-xs text-muted">Usage-based billing seat</p>}
        </div>
        {account?.plan && <Badge size="xs" color="gray" variant="light">{account.plan}</Badge>}
      </div>
      {!account && !status.authenticated && !unavailable && (
        <p className="m-0 text-xs text-muted">Run <code>/login</code> inside Claude Code, or <code>claude auth login</code> in a terminal.</p>
      )}
      {!account && (unavailable || status.authenticated) && <p className="m-0 text-xs text-muted" role="status">{status.authDetail}</p>}
      {account?.method === "subscription" && !account.usageBased && !account.id && (
        <p className="m-0 text-xs text-muted">Limits appear once Claude Code reports the account identity.</p>
      )}
      {account && account.method !== "subscription" && (
        <p className="m-0 text-xs text-muted">Subscription windows are available for Claude.ai sign-ins.</p>
      )}
      {error && <Alert color="yellow">{error}</Alert>}
      {loading && <p className="row m-0 text-xs text-muted" role="status"><Loader size={12} /> Reading account limits…</p>}
      {usage && <>
        {!hideLimits && usage.windows.length > 0 && (
          <div className="account-limits-grid">
            {usage.windows.map((window) => (
              <AccountLimit key={window.label} scope={`claude:${account?.id ?? "default"}`} label={window.label} used={window.usedPercent} resetsAt={window.resetsAt} resetText={window.resetText} />
            ))}
          </div>
        )}
        {usage.noLimits && usage.windows.length === 0 && <p className="m-0 text-xs text-muted">No usage windows reported for this plan.</p>}
        <p className="m-0 text-xs text-faint">Last checked {formatGuestDateTime(usage.refreshedAt)}</p>
      </>}
    </section>
  );
}
