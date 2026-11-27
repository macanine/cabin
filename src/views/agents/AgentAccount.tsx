import { Button, Loader } from "@mantine/core";
import { IconTerminal2 } from "@tabler/icons-react";
import { ClaudeAccount } from "../../components/ClaudeAccount";
import { CodexAccountsPanel } from "../../components/CodexAccounts";
import { AGENT_NAMES } from "../../components/common";
import type { AgentKind, AgentStatus } from "../../services/ipc/generated/core";

export const PROVIDER_NAMES: Record<AgentKind, string> = {
  claude: "Anthropic",
  codex: "OpenAI",
  antigravity: "Google",
  opencode: "OpenCode providers",
  pi: "Pi providers",
};

/** The agent's native sign-in, accounts and credentials. */
export function AgentAccount({ kind, status, onOpenTerminal, starting, hideLimits = false }: {
  kind: AgentKind;
  status?: AgentStatus;
  onOpenTerminal: (kind: AgentKind) => void;
  starting: AgentKind | null;
  hideLimits?: boolean;
}) {
  const needsSetup = !status || !status.installed || (status.cliChecked && !status.version);
  const canOpen = status?.installed && !!status.version && status.claudeLogin?.state !== "unavailable"
    && (kind === "pi" || kind === "opencode" || (kind === "antigravity" && !status.authenticated));
  return (
    <section aria-label={`${AGENT_NAMES[kind]} account`} className="grid min-w-0 gap-4">
      {!status ? <Loader size="xs" aria-label="Checking account" />
        : needsSetup ? <p className="m-0 text-sm text-muted">{status.installed ? "Reinstall" : "Install"} {AGENT_NAMES[kind]} to configure its account.</p>
        : kind === "claude" ? <ClaudeAccount status={status} hideLimits={hideLimits} />
        : kind === "codex" ? <div className="grid gap-3">
          {!status.authenticated && <p className="m-0 text-sm text-muted">Follow the sign-in prompts in the Codex terminal, or import an existing login.</p>}
          <CodexAccountsPanel status={status} hideLimits={hideLimits} />
        </div>
        : <NativeAccount status={status} />}
      {canOpen && <div><Button size="xs" variant="default" leftSection={<IconTerminal2 size={14} />}
        loading={starting === kind} disabled={starting != null} onClick={() => onOpenTerminal(kind)}>Open terminal</Button></div>}
    </section>
  );
}

function NativeAccount({ status }: { status: AgentStatus }) {
  return (
    <div className="grid gap-2 text-sm text-muted [overflow-wrap:anywhere]">
      {status.authDetail && <p className="m-0">{status.authDetail}</p>}
      {status.kind === "antigravity" && !status.authenticated && <p className="m-0">
        Follow the sign-in prompts in the Antigravity terminal. To sign in from a workspace shell, run <code>agy acp --login</code> and follow the instructions it prints.
      </p>}
      {status.kind === "opencode" && <p className="m-0">Use <code>/connect</code> in OpenCode to configure a provider.</p>}
      {status.kind === "pi" && <p className="m-0">Use <code>/login</code> in Pi to sign in to a provider.</p>}
    </div>
  );
}
