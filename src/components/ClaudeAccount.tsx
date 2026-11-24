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

export function ClaudeAccount({ status }: { status: AgentStatus }) {
  const account = currentClaudeAccount(status);
  const unavailable = status.claudeLogin?.state === "unavailable";
  const { loading, usage, error } = useClaudeUsage(account, status.installed);
  const identity = [account?.email, account?.organization].filter(Boolean).join(" · ");

  return (
    <section className="min-w-0">
      <div className="agents-section-head">
        <div className="min-w-0">
          <h3>Account &amp; limits</h3>
          {identity && <p className="font-medium text-fg [overflow-wrap:anywhere]">{identity}</p>}
          {account && <p>{account.provider ?? status.authDetail}</p>}
        </div>
        {account?.plan && <Badge size="sm" color="gray" variant="light">{account.plan}</Badge>}
      </div>
      {!account && !status.authenticated && !unavailable && <p className="agents-hint">Use <code>/login</code> in Claude Code, or run <code>claude auth login</code> from a workspace shell.</p>}
      {!account && (unavailable || status.authenticated) && <p className="agents-hint" role="status">{status.authDetail}</p>}
      {account?.usageBased && <p className="agents-hint">This seat uses usage-based billing.</p>}
      {account?.method === "subscription" && !account.usageBased && !account.id && <p className="agents-hint">Limits will appear when Claude Code reports the account identity.</p>}
      {account && account.method !== "subscription" && <p className="agents-hint">Subscription limits are available for Claude.ai sign-ins.</p>}
      {error && <Alert color="yellow" mt="sm">{error}</Alert>}
      {loading && <p className="row agents-hint" role="status"><Loader size={12} /> Reading account limits…</p>}
      {usage && <>
        {usage.windows.length > 0 && <div className="account-limits-grid">{usage.windows.map((window) => <AccountLimit key={window.label} scope={`claude:${account?.id ?? "default"}`} label={window.label} used={window.usedPercent} resetsAt={window.resetsAt} resetText={window.resetText} />)}</div>}
        {usage.noLimits && usage.windows.length === 0 && <p className="agents-hint">Claude Code reports no usage meters for this plan.</p>}
        <p className="agents-hint">Last checked {formatGuestDateTime(usage.refreshedAt)} · Reported by Claude Code</p>
      </>}
    </section>
  );
}
