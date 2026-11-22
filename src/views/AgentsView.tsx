import { useAutoRefresh } from "../lib/useAutoRefresh";
import { ActionIcon, Badge, Button, Loader, Menu, Tooltip } from "@mantine/core";
import {
  IconArrowLeft, IconDots, IconDownload, IconSettings, IconTerminal2, IconUser,
} from "@tabler/icons-react";
import { useCallback, useState } from "react";
import { AcpRuntimePanel } from "../components/AcpRuntimePanel";
import { AGENT_KINDS } from "../components/AgentPicker";
import { ClaudeAccount, useClaudeUsage } from "../components/ClaudeAccount";
import { CodexAccountsPanel } from "../components/CodexAccounts";
import { AgentLogo, AGENT_NAMES } from "../components/common";
import { AgentExtensionsPanel } from "../components/extensions/AgentExtensionsPanel";
import { MachineGate } from "../components/MachineGate";
import { NewWorkspaceModal } from "../components/NewWorkspaceModal";
import { ViewHeader } from "../components/TitleBar";
import { limitLines, type LimitLine } from "../lib/agentLimits";
import {
  AGENT_SECTIONS, agentState, attentionCount, currentClaudeAccount, defaultSection, nodeProblem,
  type AgentSection, type AgentState,
} from "../lib/agentsHub";
import { attempt, runOperation } from "../lib/operations";
import { formatDateTime } from "../lib/time";
import { agentsIpc } from "../services/ipc/agents";
import { appIpc } from "../services/ipc/app";
import type { AcpRuntime, AgentKind, AgentStatus, NodeState } from "../services/ipc/generated/core";
import { libraryIpc } from "../services/ipc/library";
import { nodeIpc } from "../services/ipc/node";
import { useApp } from "../state/store";
import { uiActions } from "../state/ui";

const BADGE: Record<AgentState["tone"], string> = { ok: "teal", info: "blue", warn: "yellow", bad: "red", off: "gray" };

function StateBadge({ state }: { state: AgentState }) {
  return <Badge size="xs" variant="light" color={BADGE[state.tone]}>{state.label}</Badge>;
}

/** Agent checks, Node.js, ACP runtimes and installs, shared by the list and an agent. */
function useAgentSetup() {
  const [node, setNode] = useState<NodeState | null>(null);
  const [runtimes, setRuntimes] = useState<AcpRuntime[]>([]);
  const [installing, setInstalling] = useState<AgentKind | null>(null);
  const loadRuntimes = useCallback(async () => {
    const next = await agentsIpc.runtimes().catch(() => null);
    if (next) setRuntimes(next);
  }, []);
  const check = useCallback(async () => {
    const [, nextNode] = await Promise.allSettled([agentsIpc.refresh(), nodeIpc.state(), loadRuntimes()]);
    if (nextNode.status === "fulfilled") setNode(nextNode.value);
  }, [loadRuntimes]);
  useAutoRefresh(() => { if (!installing) return check(); }, { minGap: 10_000, interval: 60_000 });

  const install = useCallback(async (agent: AgentStatus) => {
    setInstalling(agent.kind);
    await runOperation(appIpc.runSetup({ kind: "installAgent", agent: agent.kind }), `${AGENT_NAMES[agent.kind]} ${agent.installed ? "updated" : "installed"}`);
    await attempt(agentsIpc.refresh());
    setInstalling(null);
  }, []);

  return { node, problem: nodeProblem(node), runtimes, loadRuntimes, installing, install };
}
type Setup = ReturnType<typeof useAgentSetup>;

type Limits = { lines: LimitLine[]; plan: string | null } | "loading" | null;

/**
 * Plan limits as the providers report them (Claude Code's /usage, Codex
 * app-server's last quota). Only signed-in agents are asked; the others have
 * no limits to show.
 */
function useAgentLimits(agents: AgentStatus[], enabled: boolean): Partial<Record<AgentKind, Limits>> {
  const claude = currentClaudeAccount(agents.find((agent) => agent.kind === "claude" && agent.installed));
  const codexReady = agents.some((agent) => agent.kind === "codex" && agent.installed);
  const { usage } = useClaudeUsage(claude, enabled);
  const [codex, setCodex] = useState<Limits>(null);

  useAutoRefresh(async (isCurrent) => {
    setCodex((current) => current ?? "loading");
    try {
      const { quota } = await agentsIpc.codexAccounts();
      if (isCurrent()) setCodex(quota && !quota.error ? { lines: limitLines(quota.windows), plan: quota.planType } : null);
    } catch {
      if (isCurrent()) setCodex(null);
    }
  }, { enabled: enabled && codexReady, minGap: 60_000, interval: 180_000 });

  return {
    claude: claude ? { lines: limitLines(usage?.windows ?? []), plan: claude.plan } : null,
    codex: codexReady ? codex : null,
  };
}

/**
 * Agents, as a list of rows (logo, state, version, plan limits, the one
 * action that matters); one agent opens on Account or Setup. Token usage
 * is its own page.
 */
export function AgentsView({ agent, section }: { agent?: AgentKind; section?: AgentSection }) {
  return (
    <MachineGate>
      <AgentsContent selected={agent} section={section} />
    </MachineGate>
  );
}

function AgentsContent({ selected, section }: { selected?: AgentKind; section?: AgentSection }) {
  const agents = useApp((state) => state.agents);
  const setup = useAgentSetup();
  const limits = useAgentLimits(agents, !selected);
  const status = selected ? agents.find((entry) => entry.kind === selected) : undefined;

  if (selected) {
    return (
      <AgentDetail
        kind={selected}
        status={status}
        section={section ?? defaultSection(status)}
        setup={setup}
      />
    );
  }

  const attention = attentionCount(agents);
  return (
    <>
      <ViewHeader title="Agents" subtitle={`${agents.filter((agent) => agent.installed).length} of ${AGENT_KINDS.length} installed${attention > 0 ? ` · ${attention} need${attention === 1 ? "s" : ""} attention` : ""}`} />
      <div className="min-h-0 flex-1 overflow-y-auto px-6 pt-6 pb-12 max-sm:p-4">
        <div className="mx-auto grid max-w-[880px] min-w-0 gap-3">
          {setup.problem && (
            <div className="callout warn row">
              <span className="grow">{setup.problem}</span>
              <Button size="xs" onClick={() => uiActions.navigate({ kind: "settings", tab: "tools" })}>Set up Node.js</Button>
            </div>
          )}
          <ul className="stagger m-0 grid list-none gap-2 p-0" aria-label="Agents">
            {AGENT_KINDS.map((kind) => (
              <AgentRow
                key={kind}
                kind={kind}
                status={agents.find((entry) => entry.kind === kind)}
                limits={limits[kind]}
                setup={setup}
              />
            ))}
          </ul>
        </div>
      </div>
    </>
  );
}

function LimitText({ limits }: { limits: Limits | undefined }) {
  if (limits === "loading") return <span className="shrink-0"><Loader size={12} color="gray" /></span>;
  if (!limits || limits.lines.length === 0) return null;
  return (
    <span className="grid shrink-0 justify-items-end gap-px text-right text-sm text-muted tabular-nums max-sm:hidden">
      {limits.lines.slice(0, 3).map((line) => (
        <span key={line.text} className={line.low ? "font-semibold text-bad" : undefined}
          title={line.resetsAt != null ? `Resets ${formatDateTime(line.resetsAt)}` : undefined}>
          {line.text}
        </span>
      ))}
    </span>
  );
}

function AgentRow({ kind, status, limits, setup }: {
  kind: AgentKind;
  status: AgentStatus | undefined;
  limits: Limits | undefined;
  setup: Setup;
}) {
  const state = agentState(status);
  const open = (section?: AgentSection) => uiActions.navigate({ kind: "agents", agent: kind, section });
  const needsInstall = !status || !status.installed || (status.cliChecked && !status.version);
  const nodeBlocked = (status?.requiresNode ?? true) && setup.problem != null;
  const busy = setup.installing === kind;
  const plan = limits && limits !== "loading" ? limits.plan : null;
  const sub = !status ? "Checking…"
    : !status.installed ? status.package
      : [status.version ? `v${status.version}` : "version unavailable", status.package].join(" · ");

  return (
    <li className="glass flex min-w-0 items-center gap-2 rounded-[var(--card-radius)] pr-3 transition-colors duration-[var(--motion-fast)] hover:bg-[color-mix(in_srgb,var(--glass),var(--text)_3%)]">
      <button type="button" className="flex min-w-0 flex-1 cursor-pointer items-center gap-3 rounded-l-[var(--card-radius)] border-0 bg-transparent py-3 pr-2 pl-4 text-left text-fg focus-visible:outline-offset-[-2px]" onClick={() => open()} aria-label={`${AGENT_NAMES[kind]}: ${state.label}`}>
        <span className={`grid size-9 shrink-0 place-items-center rounded-[10px] bg-hover transition-opacity duration-300 ${state.tone === "off" ? "opacity-50" : ""}`}><AgentLogo agent={kind} size={20} /></span>
        <span className="grid min-w-0 flex-1 gap-0.5">
          <span className="flex min-w-0 items-center gap-2">
            <strong className="text-md font-semibold">{AGENT_NAMES[kind]}</strong>
            <StateBadge state={state} />
            {plan && <Badge size="xs" variant="outline" color="gray">{plan}</Badge>}
          </span>
          <span className="truncate text-sm text-muted">{sub}</span>
        </span>
        <LimitText limits={limits} />
      </button>
      <span className="flex shrink-0 items-center gap-1">
        {status && needsInstall ? (
          <Button size="xs" leftSection={<IconDownload size={14} />} loading={busy}
            disabled={setup.installing != null || nodeBlocked} onClick={() => void setup.install(status)}>
            {status.installed ? "Reinstall" : "Install"}
          </Button>
        ) : state.tone === "warn" ? (
          <Button size="xs" variant="light" onClick={() => open("account")}>Sign in</Button>
        ) : null}
        <Menu position="bottom-end" withinPortal>
          <Menu.Target>
            <ActionIcon variant="subtle" color="gray" aria-label={`${AGENT_NAMES[kind]} actions`}><IconDots size={16} /></ActionIcon>
          </Menu.Target>
          <Menu.Dropdown>
            <Menu.Item leftSection={<IconUser size={14} />} onClick={() => open("account")}>Account</Menu.Item>
            <Menu.Item leftSection={<IconSettings size={14} />} onClick={() => open("setup")}>Setup</Menu.Item>
            {status?.installed && !needsInstall && (
              <Menu.Item leftSection={<IconDownload size={14} />} disabled={setup.installing != null || nodeBlocked}
                onClick={() => void setup.install(status)}>Update CLI</Menu.Item>
            )}
          </Menu.Dropdown>
        </Menu>
      </span>
    </li>
  );
}

function AgentDetail({ kind, status, section, setup }: {
  kind: AgentKind;
  status: AgentStatus | undefined;
  section: AgentSection;
  setup: Setup;
}) {
  const state = agentState(status);
  const runtime = setup.runtimes.find((entry) => entry.agent === kind);
  const needsInstall = !status || !status.installed || (status.cliChecked && !status.version);
  const nodeBlocked = (status?.requiresNode ?? true) && setup.problem != null;
  const go = (next: AgentSection) => uiActions.navigate({ kind: "agents", agent: kind, section: next });
  const installButton = status && needsInstall && (
    <Button size="xs" leftSection={<IconDownload size={14} />} loading={setup.installing === kind}
      disabled={setup.installing != null || nodeBlocked} onClick={() => void setup.install(status)}>
      {status.installed ? "Reinstall" : "Install"}
    </Button>
  );

  return (
    <>
      <ViewHeader title={AGENT_NAMES[kind]} lead={
        <Tooltip label="All agents">
          <ActionIcon variant="subtle" color="gray" aria-label="All agents" onClick={() => uiActions.navigate({ kind: "agents" })}>
            <IconArrowLeft size={16} />
          </ActionIcon>
        </Tooltip>
      }>
        <div className="inline-flex gap-0.5 rounded-lg bg-hover p-0.5" role="tablist" aria-label={`${AGENT_NAMES[kind]} sections`}>
          {AGENT_SECTIONS.map((entry) => (
            <button key={entry.value} type="button" role="tab" aria-selected={section === entry.value} onClick={() => go(entry.value)}
              className={`press cursor-pointer rounded-md border-0 px-3 py-[3px] text-sm ${section === entry.value ? "bg-sel font-semibold text-fg" : "bg-transparent text-muted hover:text-fg"}`}>
              {entry.label}
            </button>
          ))}
        </div>
      </ViewHeader>
      <div className="min-h-0 flex-1 overflow-y-auto px-6 pt-6 pb-12 max-sm:p-4">
        <div className="mx-auto grid max-w-[880px] min-w-0 gap-3" role="tabpanel">
          {nodeBlocked && needsInstall && (
            <div className="callout warn row">
              <span className="grow">{setup.problem}</span>
              <Button size="xs" onClick={() => uiActions.navigate({ kind: "settings", tab: "tools" })}>Set up Node.js</Button>
            </div>
          )}
          {status && (
            <section className="glass flex items-center gap-4 rounded-2xl p-4">
              <span className="grid size-12 shrink-0 place-items-center rounded-xl bg-hover"><AgentLogo agent={kind} size={26} /></span>
              <div className="min-w-0 flex-1">
                <div className="flex flex-wrap items-center gap-2"><strong className="text-lg font-semibold">{AGENT_NAMES[kind]}</strong><StateBadge state={state} /></div>
                <div className="truncate text-sm text-muted">{status.version ? `v${status.version}` : status.installed ? "Version unavailable" : "Not installed"} · {status.package}</div>
                {status.configError && <div className="mt-1 text-xs text-bad">Config problem: {status.configError}</div>}
              </div>
              {needsInstall ? installButton : state.tone === "warn" ? (
                (kind !== "antigravity" || section !== "account") && (
                  <Button size="xs" variant="light" onClick={() => go("account")}>Sign in</Button>
                )
              ) : (
                <Button size="xs" variant="default" loading={setup.installing === kind} disabled={setup.installing != null || nodeBlocked} onClick={() => void setup.install(status)}>Update CLI</Button>
              )}
            </section>
          )}
          {!status ? (
            <div className="grid place-items-center py-16"><Loader size="sm" /></div>
          ) : section === "account" ? (
            !status.installed ? (
              <section className="agents-section">
                <div className="agents-section-head">
                  <div><h3>Not installed</h3><p>Install {status.package} to sign in and use {AGENT_NAMES[kind]}.</p></div>
                  {installButton}
                </div>
              </section>
            ) : kind === "claude" ? <ClaudeAccount status={status} />
              : kind === "codex" ? <section className="agents-section"><CodexAccountsPanel status={status} /></section>
                : kind === "antigravity" ? <AntigravityAccount status={status} />
                : (
                  <section className="agents-section">
                    <div className="agents-section-head"><div><h3>Account</h3><p>{status.authDetail || "Check the agent's sign-in to see its current status."}</p></div></div>
                  </section>
                )
          ) : (
            <>
              {runtime && (
                <section className="agents-section">
                  <div className="agents-section-head"><div><h3>Chat runtime</h3><p>The ACP version new chats use.</p></div></div>
                  <AcpRuntimePanel runtime={runtime} agent={status} onChange={setup.loadRuntimes} />
                </section>
              )}
              {status.installed && !needsInstall && (
                <section className="agents-section">
                  <div className="agents-section-head"><div><h3>MCP & skills</h3><p>Shared libraries, enabled per agent.</p></div>
                    <div className="flex gap-2">
                      <Button size="xs" variant="default" onClick={() => uiActions.navigate({ kind: "mcp", agent: kind })}>MCP servers</Button>
                      <Button size="xs" variant="default" onClick={() => uiActions.navigate({ kind: "skills", agent: kind })}>Skills</Button>
                    </div>
                  </div>
                  <details className="extension-project-details">
                    <summary className="cursor-pointer text-xs text-faint hover:text-muted">Project skills and agent backups</summary>
                    <AgentExtensionsPanel kind={kind} />
                  </details>
                </section>
              )}
            </>
          )}
        </div>
      </div>
    </>
  );
}

function AntigravityAccount({ status }: { status: AgentStatus }) {
  const workspace = useApp((state) => state.workspaces.find((entry) => !entry.archived));
  const [creating, setCreating] = useState(false);
  const [starting, setStarting] = useState(false);

  async function signIn(workspaceId: string) {
    setStarting(true);
    const session = await attempt(libraryIpc.createSession(workspaceId, status.kind, null));
    setStarting(false);
    if (session) uiActions.navigate({ kind: "session", id: session.id });
  }

  return (
    <section className="agents-section">
      <div className="agents-section-head">
        <div>
          <h3>Account</h3>
          {status.authDetail && <p>{status.authDetail}</p>}
          {!status.authenticated && <p>
            Follow the sign-in prompts in the Antigravity terminal. For manual sign-in, run <code>agy acp --login</code> in a workspace shell, open the URL it prints, then paste the sign-in code back into the shell.
          </p>}
        </div>
        {!status.authenticated && status.version && (
          <Button size="xs" leftSection={<IconTerminal2 size={14} />} loading={starting}
            onClick={() => workspace ? void signIn(workspace.id) : setCreating(true)}>Open terminal</Button>
        )}
      </div>
      {creating && <NewWorkspaceModal opened onClose={() => setCreating(false)} onCreated={(created) => {
        setCreating(false);
        void signIn(created.id);
      }} />}
    </section>
  );
}
