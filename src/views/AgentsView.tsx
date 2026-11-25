import { ActionIcon, Button, Loader, Tooltip } from "@mantine/core";
import { IconDownload, IconRefresh } from "@tabler/icons-react";
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
import { BurnChart, burnable } from "../components/BurnChart";
import { useMinute } from "../lib/useNow";
import { AGENT_SECTIONS, agentState, attentionCount, currentClaudeAccount, defaultSection, nodeProblem, type AgentSection, type AgentState } from "../lib/agentsHub";
import { attempt, runOperation } from "../lib/operations";
import { useAutoRefresh } from "../lib/useAutoRefresh";
import { agentsIpc } from "../services/ipc/agents";
import { appIpc } from "../services/ipc/app";
import type { AcpRuntime, AgentKind, AgentStatus, NodeState } from "../services/ipc/generated/core";
import { libraryIpc } from "../services/ipc/library";
import { nodeIpc } from "../services/ipc/node";
import { useApp } from "../state/store";
import { uiActions } from "../state/ui";
import { AgentAccount, PROVIDER_NAMES } from "./agents/AgentAccount";

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
/** `scope` names the agent and account, so recorded history never mixes accounts. */
type Limits = { windows: Window[]; plan: string | null; scope: string };

function useAgentLimits(agents: AgentStatus[], enabled: boolean): Partial<Record<AgentKind, Limits | null>> {
  const claude = currentClaudeAccount(agents.find((agent) => agent.kind === "claude" && agent.installed));
  const codexReady = agents.some((agent) => agent.kind === "codex" && agent.installed && agent.authenticated);
  const { usage } = useClaudeUsage(claude, enabled);
  const [codex, setCodex] = useState<Limits | null>(null);

  useAutoRefresh(async (isCurrent) => {
    const { quota, activeAccountId } = await agentsIpc.codexAccounts();
    if (isCurrent()) setCodex(quota && !quota.error ? { windows: quota.windows, plan: quota.planType, scope: `codex:${activeAccountId ?? "default"}` } : null);
  }, { enabled: enabled && codexReady, minGap: 60_000, interval: 180_000 });

  return {
    claude: claude ? { windows: usage?.windows ?? [], plan: claude.plan, scope: `claude:${claude.id ?? "default"}` } : null,
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

/** The agent to open on when none was asked for: one that needs you, else the first installed. */
function firstAgent(agents: AgentStatus[]): AgentKind {
  const by = (test: (agent: AgentStatus) => boolean) => AGENT_KINDS.find((kind) => agents.some((agent) => agent.kind === kind && test(agent)));
  return by((agent) => agent.installed && ["warn", "bad"].includes(agentState(agent).tone)) ?? by((agent) => agent.installed) ?? AGENT_KINDS[0];
}

const show = (agent: AgentKind, section?: AgentSection) => uiActions.navigate({ kind: "agents", agent, section });

function AgentsContent({ selected, section }: { selected?: AgentKind; section?: AgentSection }) {
  const agents = useApp((state) => state.agents);
  const setup = useAgentSetup();
  const terminal = useAgentTerminal();
  // Chosen once the agents are known, so finishing a sign-in does not switch agents.
  const [initial, setInitial] = useState<AgentKind | null>(null);
  if (initial == null && agents.length > 0) setInitial(firstAgent(agents));
  const kind = selected ?? initial ?? AGENT_KINDS[0];
  const limits = useAgentLimits(agents, true);
  const installed = agents.filter((agent) => agent.installed).length;
  const attention = attentionCount(agents);

  return (
    <>
      <ViewHeader title="Agents" subtitle={agents.length > 0 ? <>
        {installed} of {AGENT_KINDS.length} installed{attention > 0 && <span className="text-warn"> · {attention} need{attention === 1 ? "s" : ""} attention</span>}
      </> : undefined}>
        <Tooltip label="Check agent status">
          <ActionIcon size="md" variant="subtle" color="gray" aria-label="Check agent status"
            disabled={setup.installing != null} loading={setup.checking} onClick={() => void setup.check()}>
            <IconRefresh size={17} />
          </ActionIcon>
        </Tooltip>
      </ViewHeader>
      <div className="split-page">
        <nav className="split-list" aria-label="Agents">
          {AGENT_KINDS.map((entry) => {
            const status = agents.find((agent) => agent.kind === entry);
            const state = agentState(status);
            return (
              <button key={entry} type="button" className="ag-choice press" aria-current={entry === kind ? "true" : undefined} onClick={() => show(entry)}>
                <span className="ag-mark"><AgentLogo agent={entry} size={20} /></span>
                <span className="grid min-w-0 flex-1 gap-0.5">
                  <strong className="truncate text-md font-medium">{AGENT_NAMES[entry]}</strong>
                  <span className="ag-status">
                    <span className={`ag-dot ${status ? DOT[state.tone] : "bg-faint/60"}`} aria-hidden="true" />
                    <span className={status ? LABEL[state.tone] : "text-faint"}>{status ? state.label : "Checking…"}</span>
                  </span>
                  {(limits[entry]?.windows.length ?? 0) > 0 && <RowLimits windows={limits[entry]!.windows} />}
                </span>
              </button>
            );
          })}
        </nav>
        <div className="split-detail">
          <div className="split-inner">
            {setup.checkError && <p className="callout warn m-0" role="status">Couldn’t check agent status. Use Check agent status to retry.</p>}
            {setup.problem && (
              <div className="callout warn flex flex-wrap items-center justify-between gap-2">
                <span>{setup.problem}</span>
                <Button size="xs" variant="default" onClick={() => uiActions.navigate({ kind: "settings", tab: "tools" })}>Set up Node.js</Button>
              </div>
            )}
            <AgentDetail key={kind} kind={kind} section={section} status={agents.find((entry) => entry.kind === kind)}
              limits={limits[kind]} setup={setup} onOpenTerminal={terminal.open} starting={terminal.starting} />
          </div>
        </div>
      </div>
      {terminal.picker}
    </>
  );
}

const LEFT = (forecast: LimitForecast) => Math.max(0, Math.round(100 - forecast.used));

/** The agent's plan limits: what is left, when it resets, and where the pace lands. */
function PlanLimits({ kind, limits }: { kind: AgentKind; limits: Limits }) {
  const now = useMinute();
  return (
    <section className="ag-limits" aria-label="Plan limits">
      <header className="ag-limits-head">
        <h3>Plan limits</h3>
        <span>Forecasts assume the pace since each window opened</span>
      </header>
      <div className="ag-limit-tiles stagger">
        {limits.windows.map((window) => {
          const forecast = forecastLimit(window, now);
          const name = `${AGENT_NAMES[kind]} · ${shortLimitLabel(window.label)}`;
          return (
            <div key={window.label} className="ag-tile" data-tone={forecast.tone} role="group" aria-label={`${name}: ${LEFT(forecast)}% left. ${forecast.text}`}>
              <span className="ag-tile-name">{shortLimitLabel(window.label)}</span>
              <span className="ag-tile-figure">
                <strong>{LEFT(forecast)}<small>%</small></strong>
                <span>left</span>
                {forecast.resetsIn != null && <span className="ag-tile-reset">resets in {formatSpan(forecast.resetsIn)}</span>}
              </span>
              {burnable(window, now)
                ? <BurnChart window={window} forecast={forecast} scope={limits.scope} now={now} height={54} />
                : <PaceMeter forecast={forecast} label={name} />}
              <span className="ag-tile-forecast">{forecast.text}</span>
            </div>
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

/** One agent: its state and actions, its plan limits, then its account or setup. */
function AgentDetail({ kind, section, status, limits, setup, onOpenTerminal, starting }: {
  kind: AgentKind;
  section?: AgentSection;
  status?: AgentStatus;
  limits?: Limits | null;
  setup: Setup;
  onOpenTerminal: (kind: AgentKind) => void;
  starting: AgentKind | null;
}) {
  const state = agentState(status);
  const nodeBlocked = (status?.requiresNode ?? true) && setup.problem != null;
  const canOperate = status?.cliChecked && status.installed && !!status.version;
  const needsInstall = status && (!status.installed || (status.cliChecked && !status.version));
  const account = currentClaudeAccount(status);
  const provider = account?.provider || PROVIDER_NAMES[kind];
  const tab = section ?? defaultSection(status);
  return (
    <>
      <header className="ag-head">
        <span className="ag-head-mark"><AgentLogo agent={kind} size={28} /></span>
        <div className="min-w-0 flex-1">
          <h2>{AGENT_NAMES[kind]}</h2>
          <p>
            <span className={status ? LABEL[state.tone] : "text-faint"}>{status ? state.label : "Checking…"}</span>
            {status?.version && <span className="tabular-nums"> · v{status.version}</span>}
            {status?.installed && <> · {provider}{limits?.plan && ` ${limits.plan}`}</>}
          </p>
        </div>
        <div className="flex flex-wrap items-center gap-1.5">
          {canOperate && <Button size="xs" variant="default" loading={setup.installing === kind}
            disabled={setup.installing != null || nodeBlocked} onClick={() => void setup.install(status)}>Update CLI</Button>}
          {needsInstall && <Button size="xs" leftSection={<IconDownload size={14} />}
            loading={setup.installing === kind} disabled={setup.installing != null || nodeBlocked}
            onClick={() => void setup.install(status)}>{status.installed ? "Reinstall" : "Install"}</Button>}
          {canOperate && state.tone === "warn" && <Button size="xs" loading={starting === kind}
            disabled={starting != null} onClick={() => onOpenTerminal(kind)}>Sign in</Button>}
        </div>
      </header>
      {/* The account shows the same windows with its own controls. */}
      {tab === "setup" && limits && limits.windows.length > 0 && <PlanLimits kind={kind} limits={limits} />}
      <div className="ag-panel">
        <nav className="ag-tabs" aria-label={`${AGENT_NAMES[kind]} sections`}>
          {AGENT_SECTIONS.map(({ value, label }) => (
            <button key={value} type="button" className="agents-nav-button" aria-current={tab === value ? "page" : undefined} onClick={() => show(kind, value)}>{label}</button>
          ))}
        </nav>
        {tab === "account"
          ? <AgentAccount kind={kind} status={status} onOpenTerminal={onOpenTerminal} starting={starting} />
          : <AgentSetupDetails kind={kind} status={status} setup={setup} />}
      </div>
    </>
  );
}

function AgentSetupDetails({ kind, status, setup }: { kind: AgentKind; status?: AgentStatus; setup: Setup }) {
  const runtime = setup.runtimes.find((entry) => entry.agent === kind);
  return (
    <section aria-label={`${AGENT_NAMES[kind]} setup`} className="grid min-w-0 gap-5 p-5 max-sm:p-4">
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
