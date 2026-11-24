import { Badge, Button, Loader } from "@mantine/core";
import { IconChevronRight, IconSettings, IconTerminal2 } from "@tabler/icons-react";
import { AGENT_KINDS } from "../../components/AgentPicker";
import { ClaudeAccount } from "../../components/ClaudeAccount";
import { CodexAccountsPanel } from "../../components/CodexAccounts";
import { AgentLogo, AGENT_NAMES } from "../../components/common";
import { agentState, currentClaudeAccount } from "../../lib/agentsHub";
import type { AgentKind, AgentStatus } from "../../services/ipc/generated/core";

export const PROVIDER_NAMES: Record<AgentKind, string> = {
  claude: "Anthropic",
  codex: "OpenAI",
  antigravity: "Google",
  opencode: "OpenCode providers",
  pi: "Pi providers",
};

export function AgentProviders({ agents, selected, onSelect, onSetup, onOpenTerminal, starting }: {
  agents: AgentStatus[];
  selected?: AgentKind;
  onSelect: (kind: AgentKind) => void;
  onSetup: (kind: AgentKind) => void;
  onOpenTerminal: (kind: AgentKind) => void;
  starting: AgentKind | null;
}) {
  const kind = selected ?? AGENT_KINDS.find((entry) => agents.some((agent) => agent.kind === entry && agent.installed)) ?? AGENT_KINDS[0];
  const status = agents.find((agent) => agent.kind === kind);
  const state = agentState(status);
  const provider = currentClaudeAccount(status)?.provider || PROVIDER_NAMES[kind];
  const needsSetup = !status || !status.installed || (status.cliChecked && !status.version);
  const canOpen = status?.installed && !!status.version && status.claudeLogin?.state !== "unavailable"
    && (!status.authenticated || kind === "pi" || kind === "opencode");

  return (
    <div className="agents-provider-layout">
      <nav className="agents-provider-list" aria-label="Providers">
        {AGENT_KINDS.map((entry) => {
          const agent = agents.find((item) => item.kind === entry);
          const account = currentClaudeAccount(agent);
          return (
            <button key={entry} type="button" className="agents-provider-choice"
              aria-current={kind === entry ? "true" : undefined} onClick={() => onSelect(entry)}>
              <span className="grid min-w-0 gap-0.5">
                <span className="font-semibold text-fg [overflow-wrap:anywhere]">{account?.provider || PROVIDER_NAMES[entry]}</span>
                <span className="text-xs text-muted">{AGENT_NAMES[entry]}</span>
              </span>
              <IconChevronRight size={15} className="agents-provider-chevron shrink-0" aria-hidden="true" />
            </button>
          );
        })}
      </nav>
      <section aria-labelledby="agent-provider-title" className="min-w-0 rounded-xl border border-line bg-glass">
        <header className="flex flex-wrap items-center gap-3 border-b border-line px-4 py-4 sm:px-5">
          <span className="grid size-10 shrink-0 place-items-center rounded-lg bg-hover"><AgentLogo agent={kind} size={22} /></span>
          <div className="min-w-0 flex-1">
            <h2 id="agent-provider-title" className="m-0 text-lg font-semibold [overflow-wrap:anywhere]">{provider}</h2>
            <p className="m-0 mt-0.5 text-sm text-muted">{AGENT_NAMES[kind]}</p>
          </div>
          {status && <Badge size="sm" variant="light" color={{ ok: "teal", info: "blue", warn: "yellow", bad: "red", off: "gray" }[state.tone]}>{state.label}</Badge>}
          {status && needsSetup && <Button size="xs" variant="default" leftSection={<IconSettings size={14} />}
            onClick={() => onSetup(kind)}>Open setup</Button>}
          {canOpen && <Button size="xs" variant="light" leftSection={<IconTerminal2 size={14} />}
            loading={starting === kind} disabled={starting != null} onClick={() => onOpenTerminal(kind)}>Open terminal</Button>}
        </header>
        <div key={kind} className="min-w-0 p-4 sm:p-5">
          {!status ? <Loader size="xs" aria-label="Checking provider" />
            : needsSetup ? <p className="m-0 text-sm text-muted">{status.installed ? "Reinstall" : "Install"} {AGENT_NAMES[kind]} to use this connection.</p>
            : kind === "claude" ? <ClaudeAccount status={status} />
            : kind === "codex" ? <>
              {!status.authenticated && <p className="mt-0 mb-4 text-sm text-muted">Follow the sign-in prompts in the Codex terminal, or import an existing login.</p>}
              <CodexAccountsPanel status={status} />
            </>
            : <NativeAccount status={status} />}
        </div>
      </section>
    </div>
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
