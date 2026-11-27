import { Alert, Badge, Loader } from "@mantine/core";
import { currentClaudeAccount } from "../lib/agentsHub";
import { formatGuestDateTime } from "../lib/time";
import { useUnifiedQuotas } from "../state/quotaStore";
import type { AgentStatus } from "../services/ipc/generated/core";
import { AccountLimit } from "./AccountLimits";

export function ClaudeAccount({ status, hideLimits = false }: { status: AgentStatus; hideLimits?: boolean }) {
  const account = currentClaudeAccount(status);
  const unavailable = status.claudeLogin?.state === "unavailable";
  const quotas = useUnifiedQuotas(status.installed);
  const summary = quotas.byAgent.claude;
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
      {!hideLimits && summary.error && <Alert color="yellow">{summary.error}</Alert>}
      {!hideLimits && summary.loading && <p className="row m-0 text-xs text-muted" role="status"><Loader size={12} /> Reading account limits…</p>}
      {!hideLimits && summary.windows.length > 0 && (
        <div className="account-limits-grid">
          {summary.windows.map((window) => (
            <AccountLimit
              key={window.id}
              scope={window.scope}
              label={window.label}
              used={window.usedPercent}
              resetsAt={window.resetsAt}
              resetText={window.resetText}
            />
          ))}
        </div>
      )}
      {!hideLimits && summary.noLimits && summary.windows.length === 0 && (
        <p className="m-0 text-xs text-muted">No usage windows reported for this plan.</p>
      )}
      {summary.refreshedAt != null && (
        <p className="m-0 text-xs text-faint">Last checked {formatGuestDateTime(summary.refreshedAt)}</p>
      )}
    </section>
  );
}
