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

/** The agent's native sign-in, accounts and limits. */
export function AgentAccount({ kind, status, onOpenTerminal, starting }: {
  kind: AgentKind;
  status?: AgentStatus;
  onOpenTerminal: (kind: AgentKind) => void;
  starting: AgentKind | null;
}) {
  const needsSetup = !status || !status.installed || (status.cliChecked && !status.version);
  // The header offers Sign in when that is what is missing.
  const canOpen = status?.installed && !!status.version && status.claudeLogin?.state !== "unavailable"
    && (kind === "pi" || kind === "opencode");
  return (
    <section aria-label={`${AGENT_NAMES[kind]} account`} className="grid min-w-0 gap-4 p-5 max-sm:p-4">
      {!status ? <Loader size="xs" aria-label="Checking account" />
        : needsSetup ? <p className="m-0 text-sm text-muted">{status.installed ? "Reinstall" : "Install"} {AGENT_NAMES[kind]} to use this connection.</p>
        : kind === "claude" ? <ClaudeAccount status={status} />
        : kind === "codex" ? <div>
          {!status.authenticated && <p className="mt-0 mb-4 text-sm text-muted">Follow the sign-in prompts in the Codex terminal, or import an existing login.</p>}
          <CodexAccountsPanel status={status} />
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
