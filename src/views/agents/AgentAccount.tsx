import { Button, Loader } from "@mantine/core";
import { IconTerminal2 } from "@tabler/icons-react";
import { ClaudeAccount } from "../../components/ClaudeAccount";
import { CodexAccountsPanel } from "../../components/CodexAccounts";
import { AGENT_NAMES } from "../../components/common";
import { useI18n } from "../../i18n";
import { needsCliLogin } from "../../lib/agentsHub";
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
  onOpenTerminal: (kind: AgentKind, login: boolean) => void;
  starting: AgentKind | null;
  hideLimits?: boolean;
}) {
  const { t } = useI18n();
  const needsSetup = !status || !status.installed || (status.cliChecked && !status.version);
  const login = status != null && needsCliLogin(status);
  const canAddProvider = !login && status?.installed && !!status.version && !status.configError && (kind === "pi" || kind === "opencode");
  return (
    <section aria-label={`${AGENT_NAMES[kind]} account`} className="grid min-w-0 gap-4">
      {login && <p className="m-0 text-sm text-muted">{t("agents.loginIntro", { agent: AGENT_NAMES[kind] })}</p>}
      {!status ? <Loader size="xs" aria-label="Checking account" />
        : needsSetup ? <p className="m-0 text-sm text-muted">{status.installed ? "Reinstall" : "Install"} {AGENT_NAMES[kind]} to configure its account.</p>
        : kind === "claude" ? <ClaudeAccount status={status} hideLimits={hideLimits} />
        : kind === "codex" ? <CodexAccountsPanel status={status} hideLimits={hideLimits} />
        : !login && status.authDetail && <p className="m-0 text-sm text-muted [overflow-wrap:anywhere]">{status.authDetail}</p>}
      {canAddProvider && <div><Button size="xs" variant="default" leftSection={<IconTerminal2 size={14} />}
        loading={starting === kind} disabled={starting != null} onClick={() => onOpenTerminal(kind, true)}>{t("agents.addProvider")}</Button></div>}
    </section>
  );
}
