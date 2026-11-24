import { ActionIcon, Button, Loader, Tooltip } from "@mantine/core";
import { IconChevronDown, IconChevronRight, IconDownload, IconRefresh } from "@tabler/icons-react";
import { useCallback, useRef, useState } from "react";
import { AcpRuntimePanel } from "../components/AcpRuntimePanel";
import { AGENT_KINDS } from "../components/AgentPicker";
import { useClaudeUsage } from "../components/ClaudeAccount";
import { AgentLogo, AGENT_NAMES } from "../components/common";
import { AgentExtensionsPanel } from "../components/extensions/AgentExtensionsPanel";
import { MachineGate } from "../components/MachineGate";
import { NewWorkspaceModal } from "../components/NewWorkspaceModal";
import { ViewHeader } from "../components/TitleBar";
import { forecastLimit, formatSpan, shortLimitLabel, type LimitForecast, type LimitWindow } from "../lib/agentLimits";
import { PaceMeter } from "../components/PaceMeter";
import { useMinute } from "../lib/useNow";
import { agentState, attentionCount, currentClaudeAccount, defaultSection, nodeProblem, type AgentSection, type AgentState } from "../lib/agentsHub";
import { attempt, runOperation } from "../lib/operations";
import { useAutoRefresh } from "../lib/useAutoRefresh";
import { agentsIpc } from "../services/ipc/agents";
import { appIpc } from "../services/ipc/app";
import type { AcpRuntime, AgentKind, AgentStatus, NodeState } from "../services/ipc/generated/core";
import { libraryIpc } from "../services/ipc/library";
import { nodeIpc } from "../services/ipc/node";
import { useApp } from "../state/store";
import { uiActions } from "../state/ui";
import { AgentProviders, PROVIDER_NAMES } from "./agents/AgentProviders";

function useAgentSetup() {
  const [node, setNode] = useState<NodeState | null>(null);
  const [runtimes, setRuntimes] = useState<AcpRuntime[]>([]);
  const [installing, setInstalling] = useState<AgentKind | null>(null);
  const [checking, setChecking] = useState(false);
  const [checkError, setCheckError] = useState(false);
  const pendingCheck = useRef<Promise<void> | null>(null);
  const loadRuntimes = useCallback(async () => {
    const next = await agentsIpc.runtimes().catch(() => null);
    if (next) setRuntimes(next);
  }, []);
  const check = useCallback(() => {
    if (pendingCheck.current) return pendingCheck.current;
    setChecking(true);
    pendingCheck.current = Promise.allSettled([agentsIpc.refresh(), nodeIpc.state(), loadRuntimes()])
      .then(([agentCheck, nodeCheck]) => {
        setCheckError(agentCheck.status === "rejected");
        if (nodeCheck.status === "fulfilled") setNode(nodeCheck.value);
      }).finally(() => {
        pendingCheck.current = null;
        setChecking(false);
      });
    return pendingCheck.current;
  }, [loadRuntimes]);
  useAutoRefresh(check, { enabled: installing == null, minGap: 10_000, interval: 60_000 });

  async function install(agent: AgentStatus) {
    setInstalling(agent.kind);
    try {
      const action = !agent.installed ? "installed" : agent.version ? "updated" : "reinstalled";
      await runOperation(appIpc.runSetup({ kind: "installAgent", agent: agent.kind }), `${AGENT_NAMES[agent.kind]} ${action}`);
    } finally {
      setInstalling(null);
    }
  }

  return { problem: nodeProblem(node), runtimes, loadRuntimes, installing, install, checking, checkError, check };
}
type Setup = ReturnType<typeof useAgentSetup>;

type Window = LimitWindow & { windowMinutes?: number | null };
type Limits = { windows: Window[]; plan: string | null };

function useAgentLimits(agents: AgentStatus[], enabled: boolean): Partial<Record<AgentKind, Limits | null>> {
  const claude = currentClaudeAccount(agents.find((agent) => agent.kind === "claude" && agent.installed));
  const codexReady = agents.some((agent) => agent.kind === "codex" && agent.installed && agent.authenticated);
  const { usage } = useClaudeUsage(claude, enabled);
  const [codex, setCodex] = useState<Limits | null>(null);

  useAutoRefresh(async (isCurrent) => {
    const { quota } = await agentsIpc.codexAccounts();
    if (isCurrent()) setCodex(quota && !quota.error ? { windows: quota.windows, plan: quota.planType } : null);
  }, { enabled: enabled && codexReady, minGap: 60_000, interval: 180_000 });

  return {
    claude: claude ? { windows: usage?.windows ?? [], plan: claude.plan } : null,
    codex: codexReady ? codex : null,
  };
}

function useAgentTerminal() {
  const workspace = useApp((state) => state.workspaces.find((entry) => !entry.archived));
  const [creating, setCreating] = useState<AgentKind | null>(null);
  const [starting, setStarting] = useState<AgentKind | null>(null);

  async function open(kind: AgentKind, workspaceId: string) {
    setStarting(kind);
    try {
      const session = await attempt(libraryIpc.createSession(workspaceId, kind, null));
      if (session) uiActions.navigate({ kind: "session", id: session.id });
    } finally {
      setStarting(null);
    }
  }

  return {
    starting,
    open: (kind: AgentKind) => workspace ? void open(kind, workspace.id) : setCreating(kind),
    picker: creating && <NewWorkspaceModal opened onClose={() => setCreating(null)} onCreated={(created) => {
      setCreating(null);
      void open(creating, created.id);
    }} />,
  };
}

export function AgentsView({ agent, section }: { agent?: AgentKind; section?: AgentSection }) {
  return <MachineGate><AgentsContent selected={agent} section={section} /></MachineGate>;
}

function showAgents(agent?: AgentKind) {
  uiActions.navigate(agent ? { kind: "agents", agent, section: "setup" } : { kind: "agents" });
}

function showProviders(agent?: AgentKind) {
  uiActions.navigate(agent ? { kind: "agents", agent, section: "account" } : { kind: "agents", section: "account" });
}

function AgentsContent({ selected, section }: { selected?: AgentKind; section?: AgentSection }) {
  const agents = useApp((state) => state.agents);
  const setup = useAgentSetup();
  const terminal = useAgentTerminal();
  const status = agents.find((agent) => agent.kind === selected);
  const page = section ?? (selected ? defaultSection(status) : "setup");
  const limits = useAgentLimits(agents, page === "setup");
  const installed = agents.filter((agent) => agent.installed).length;
  const attention = attentionCount(agents);

  return (
    <>
      <ViewHeader title="Agents & providers">
        <Tooltip label="Check agent status">
          <ActionIcon size="md" variant="subtle" color="gray" aria-label="Check agent status"
            disabled={setup.installing != null} loading={setup.checking} onClick={() => void setup.check()}>
            <IconRefresh size={17} />
          </ActionIcon>
        </Tooltip>
      </ViewHeader>
      <div className="min-h-0 flex-1 overflow-y-auto px-6 pt-5 pb-12 max-sm:px-3">
        <div className="agents-workspace mx-auto max-w-[1120px]">
          <div className="mb-4 flex flex-wrap items-end justify-between gap-x-4 gap-y-2 border-b border-line">
            <nav className="flex gap-4" aria-label="Agent management">
              <button type="button" className="agents-nav-button" aria-current={page === "setup" ? "page" : undefined}
                onClick={() => showAgents()}>Agents</button>
              <button type="button" className="agents-nav-button" aria-current={page === "account" ? "page" : undefined}
                onClick={() => showProviders(status?.kind)}>Providers</button>
            </nav>
            {page === "setup" && agents.length > 0 && <p className="m-0 pb-2.5 text-sm text-muted">
              {installed} of {AGENT_KINDS.length} installed
              {attention > 0 && <span className="ml-3 text-warn">{attention} need{attention === 1 ? "s" : ""} attention</span>}
            </p>}
          </div>
          {setup.checkError && <p className="callout warn mb-3" role="status">Couldn’t check agent status. Use Check agent status to retry.</p>}
          {page === "account" ? (
            <AgentProviders agents={agents} selected={selected} onSelect={showProviders} onSetup={showAgents}
              onOpenTerminal={terminal.open} starting={terminal.starting} />
          ) : (
            <>
              {setup.problem && (
                <div className="callout warn mb-3 flex flex-wrap items-center justify-between gap-2">
                  <span>{setup.problem}</span>
                  <Button size="xs" variant="default" onClick={() => uiActions.navigate({ kind: "settings", tab: "tools" })}>Set up Node.js</Button>
                </div>
              )}
              <LimitsOverview limits={limits} />
              <div className="ag-list">
                <ul className="m-0 list-none p-0" aria-label="Agents">
                  {AGENT_KINDS.map((kind) => (
                    <AgentRow key={kind} kind={kind} status={agents.find((entry) => entry.kind === kind)}
                      limits={limits[kind]} setup={setup} expanded={selected === kind} onOpenTerminal={terminal.open} starting={terminal.starting} />
                  ))}
                </ul>
              </div>
            </>
          )}
        </div>
      </div>
      {terminal.picker}
    </>
  );
}

type Reading = { kind: AgentKind; window: Window; forecast: LimitForecast };

/** Every plan window the agents report, with its pace forecast, tightest first per agent. */
function useReadings(limits: Partial<Record<AgentKind, Limits | null>>): Reading[] {
  const now = useMinute();
  return AGENT_KINDS.flatMap((kind) => (limits[kind]?.windows ?? []).map((window) => ({ kind, window, forecast: forecastLimit(window, now) })));
}

const LEFT = (forecast: LimitForecast) => Math.max(0, Math.round(100 - forecast.used));

/** Plan limits across agents: what is left, when it resets, and where the pace lands. */
function LimitsOverview({ limits }: { limits: Partial<Record<AgentKind, Limits | null>> }) {
  const readings = useReadings(limits);
  if (readings.length === 0) return null;
  return (
    <section className="ag-limits" aria-label="Plan limits">
      <header className="ag-limits-head">
        <h2>Plan limits</h2>
        <span>Forecasts assume the pace since each window opened</span>
      </header>
      <div className="ag-limit-tiles stagger">
        {readings.map(({ kind, window, forecast }) => {
          const name = `${AGENT_NAMES[kind]} · ${shortLimitLabel(window.label)}`;
          return (
            <button key={`${kind}-${window.label}`} type="button" className="ag-tile" data-tone={forecast.tone}
              onClick={() => showProviders(kind)} aria-label={`${name}: ${LEFT(forecast)}% left. ${forecast.text}`}>
              <span className="ag-tile-name">{name}</span>
              <span className="ag-tile-figure">
                <strong>{LEFT(forecast)}<small>%</small></strong>
                <span>left</span>
                {forecast.resetsIn != null && <span className="ag-tile-reset">resets in {formatSpan(forecast.resetsIn)}</span>}
              </span>
              <PaceMeter forecast={forecast} label={name} />
              <span className="ag-tile-forecast">{forecast.text}</span>
            </button>
          );
        })}
      </div>
    </section>
  );
}

/** The row's windows as short meters; the overview above carries the detail. */
function RowLimits({ windows }: { windows: Window[] }) {
  const now = useMinute();
  return (
    <span className="ag-row-meters">
      {windows.slice(0, 2).map((window) => {
        const forecast = forecastLimit(window, now);
        const label = shortLimitLabel(window.label);
        return (
          <span key={window.label} className="ag-row-meter" data-tone={forecast.tone} title={`${label}: ${LEFT(forecast)}% left · ${forecast.text}`}>
            <span className="ag-row-meter-label">{label.replace("Weekly", "Week")}</span>
            <PaceMeter forecast={forecast} size="sm" label={`${label} ${LEFT(forecast)}% left`} />
            <span className="ag-row-meter-value">{LEFT(forecast)}%</span>
          </span>
        );
      })}
    </span>
  );
}

const DOT: Record<AgentState["tone"], string> = {
  ok: "bg-ok", info: "bg-info", warn: "bg-warn", bad: "bg-bad", off: "bg-faint/60",
};
const LABEL: Record<AgentState["tone"], string> = {
  ok: "text-muted", info: "text-info", warn: "text-warn", bad: "text-bad", off: "text-faint",
};

function AgentRow({ kind, status, limits, setup, expanded, onOpenTerminal, starting }: {
  kind: AgentKind;
  status?: AgentStatus;
  limits?: Limits | null;
  setup: Setup;
  expanded: boolean;
  onOpenTerminal: (kind: AgentKind) => void;
  starting: AgentKind | null;
}) {
  const state = agentState(status);
  const nodeBlocked = (status?.requiresNode ?? true) && setup.problem != null;
  const canOperate = status?.cliChecked && status.installed && !!status.version;
  const needsInstall = status && (!status.installed || (status.cliChecked && !status.version));
  const detailId = `agent-setup-${kind}`;
  const account = currentClaudeAccount(status);
  const provider = account?.provider || PROVIDER_NAMES[kind];

  return (
    <li aria-label={AGENT_NAMES[kind]} className="ag-item" data-expanded={expanded || undefined}>
      <div className="ag-row">
        <div className="ag-id">
          <span className="ag-mark"><AgentLogo agent={kind} size={22} /></span>
          <div className="grid min-w-0 gap-0.5">
            <strong className="text-md font-semibold">{AGENT_NAMES[kind]}</strong>
            <span className="ag-status">
              <span className={`ag-dot ${status ? DOT[state.tone] : "bg-faint/60"}`} aria-hidden="true" />
              <span className={status ? LABEL[state.tone] : "text-faint"}>{status ? state.label : "Checking…"}</span>
              {status?.version && <span className="text-faint tabular-nums">v{status.version}</span>}
            </span>
          </div>
        </div>
        <div className="ag-provider">
          {status?.installed && <button type="button" className="ag-provider-link"
            aria-label={`Manage ${AGENT_NAMES[kind]} account`} onClick={() => showProviders(kind)}>
            <span className="ag-provider-name">
              <span>{provider}</span>
              {limits?.plan && <span className="ag-plan">{limits.plan}</span>}
              <IconChevronRight size={13} className="shrink-0 text-faint" aria-hidden="true" />
            </span>
            {limits && limits.windows.length > 0 && <RowLimits windows={limits.windows} />}
          </button>}
        </div>
        <div className="ag-actions">
          {needsInstall && <Button size="xs" leftSection={<IconDownload size={14} />}
            loading={setup.installing === kind} disabled={setup.installing != null || nodeBlocked}
            onClick={() => void setup.install(status)}>{status.installed ? "Reinstall" : "Install"}</Button>}
          {canOperate && state.tone === "warn" && <Button size="xs" variant="light" loading={starting === kind}
            disabled={starting != null} onClick={() => onOpenTerminal(kind)}>Sign in</Button>}
          {canOperate && <Button size="xs" variant="subtle" color="gray" loading={setup.installing === kind}
            disabled={setup.installing != null || nodeBlocked} onClick={() => void setup.install(status)}>Update CLI</Button>}
          <Button size="xs" variant="subtle" color="gray" aria-expanded={expanded} aria-controls={expanded ? detailId : undefined}
            aria-label={`${AGENT_NAMES[kind]} setup`} rightSection={<IconChevronDown size={13} className={`transition-transform duration-[var(--motion-reveal)] ${expanded ? "rotate-180" : ""}`} />}
            onClick={() => showAgents(expanded ? undefined : kind)}>Setup</Button>
        </div>
      </div>
      {expanded && <AgentSetupDetails id={detailId} kind={kind} status={status} setup={setup} />}
    </li>
  );
}

function AgentSetupDetails({ id, kind, status, setup }: { id: string; kind: AgentKind; status?: AgentStatus; setup: Setup }) {
  const runtime = setup.runtimes.find((entry) => entry.agent === kind);
  return (
    <section id={id} aria-label={`${AGENT_NAMES[kind]} setup`} className="ag-setup grid min-w-0 gap-4 px-4 py-4 sm:px-5">
      {!status ? <Loader size="xs" aria-label="Checking installation" /> : <>
        <div className="grid gap-1 text-sm text-muted">
          <span className="[overflow-wrap:anywhere]">Package <code className="ml-1 text-fg">{status.package}</code></span>
          {status.configError && <span className="text-bad [overflow-wrap:anywhere]">{status.configError}</span>}
        </div>
        {runtime && <div className="min-w-0">
          <h3 className="m-0 mb-2 text-md font-semibold">Chat runtime</h3>
          <AcpRuntimePanel runtime={runtime} agent={status} onChange={setup.loadRuntimes} />
        </div>}
        {status.installed && status.version && <div className="grid gap-2">
          <div className="flex flex-wrap items-center justify-between gap-2">
            <h3 className="m-0 text-md font-semibold">MCP &amp; skills</h3>
            <div className="flex gap-2">
              <Button size="xs" variant="default" onClick={() => uiActions.navigate({ kind: "mcp", agent: kind })}>MCP servers</Button>
              <Button size="xs" variant="default" onClick={() => uiActions.navigate({ kind: "skills", agent: kind })}>Skills</Button>
            </div>
          </div>
          <details className="extension-project-details">
            <summary className="cursor-pointer text-sm text-muted">Project skills and agent backups</summary>
            <AgentExtensionsPanel kind={kind} />
          </details>
        </div>}
      </>}
    </section>
  );
}
