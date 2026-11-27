import { Button, Loader, SegmentedControl, Tooltip } from "@mantine/core";
import { IconDownload, IconPlug, IconRefresh, IconSparkles, IconTerminal2 } from "@tabler/icons-react";
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
const LEFT = (forecast: LimitForecast) => Math.max(0, Math.round(100 - forecast.used));

const PILL_TONE: Record<AgentState["tone"], string> = {
  ok: "ok", info: "info", warn: "warn", bad: "bad", off: "",
};

/** Compact remaining-quota bars inside each top overview card. */
function CardMeters({ windows }: { windows: Window[] }) {
  const now = useMinute();
  return (
    <div className="ag-card-meters">
      {windows.slice(0, 2).map((window) => {
        const forecast = forecastLimit(window, now);
        const label = shortLimitLabel(window.label);
        return (
          <div key={window.label} className="ag-card-meter" data-tone={forecast.tone} title={`${label}: ${LEFT(forecast)}% left · ${forecast.text}`}>
            <span className="ag-card-meter-label">{label.replace("Weekly", "Week")}</span>
            <PaceMeter forecast={forecast} size="sm" label={`${label} ${LEFT(forecast)}% left`} />
            <span className="ag-card-meter-value">{LEFT(forecast)}%</span>
          </div>
        );
      })}
    </div>
  );
}

/**
 * Single-canvas Agents control center: a responsive 5-card overview deck on top
 * (all agents, versions and live quotas at one glance), followed by the focused
 * agent's actions, live burn charts, account controls and runtime setup.
 */
function AgentsContent({ selected, section }: { selected?: AgentKind; section?: AgentSection }) {
  const agents = useApp((state) => state.agents);
  const setup = useAgentSetup();
  const terminal = useAgentTerminal();
  const [initial, setInitial] = useState<AgentKind | null>(null);
  if (initial == null && agents.length > 0) setInitial(firstAgent(agents));
  const kind = selected ?? initial ?? AGENT_KINDS[0];
  const limits = useAgentLimits(agents, true);
  const installed = agents.filter((agent) => agent.installed).length;
  const attention = attentionCount(agents);

  return (
    <>
      <ViewHeader
        title="Agents"
        subtitle={agents.length > 0 ? (
          <>
            {installed} of {AGENT_KINDS.length} installed
            {attention > 0 && <span className="text-warn"> · {attention} need{attention === 1 ? "s" : ""} attention</span>}
          </>
        ) : undefined}
      >
        <Button
          size="xs"
          variant="default"
          leftSection={<IconRefresh size={14} />}
          disabled={setup.installing != null}
          loading={setup.checking}
          onClick={() => void setup.check()}
        >
          Refresh
        </Button>
      </ViewHeader>
      <div className="ag-page">
        <div className="ag-inner">
          {setup.checkError && <p className="callout warn m-0" role="status">Couldn’t check agent status. Select Refresh to try again.</p>}
          {setup.problem && (
            <div className="callout warn flex flex-wrap items-center justify-between gap-2">
              <span>{setup.problem}</span>
              <Button size="xs" variant="default" onClick={() => uiActions.navigate({ kind: "settings", tab: "tools" })}>Set up Node.js</Button>
            </div>
          )}

          <nav className="ag-deck stagger" aria-label="Agents">
            {AGENT_KINDS.map((entry) => {
              const status = agents.find((agent) => agent.kind === entry);
              const state = agentState(status);
              const active = entry === kind;
              const entryLimits = limits[entry];
              return (
                <button
                  key={entry}
                  type="button"
                  className="ag-card press"
                  aria-current={active ? "true" : undefined}
                  data-installed={status?.installed ? "true" : "false"}
                  onClick={() => show(entry)}
                >
                  <div className="ag-card-top">
                    <span className="ag-card-logo"><AgentLogo agent={entry} size={20} /></span>
                    <span className={`pill ag-card-pill ${status ? PILL_TONE[state.tone] : ""}`}>
                      {status ? state.label : "Checking…"}
                    </span>
                  </div>
                  <div className="ag-card-name">
                    <strong>{AGENT_NAMES[entry]}</strong>
                    <span>
                      {status?.version ? `v${status.version}` : PROVIDER_NAMES[entry]}
                      {entryLimits?.plan ? ` · ${entryLimits.plan}` : ""}
                    </span>
                  </div>
                  {(entryLimits?.windows.length ?? 0) > 0 ? (
                    <CardMeters windows={entryLimits!.windows} />
                  ) : (
                    <div className="ag-card-foot">
                      {!status ? "Checking CLI…"
                        : !status.installed ? "Not installed"
                        : status.authenticated ? PROVIDER_NAMES[entry]
                        : "Needs sign-in"}
                    </div>
                  )}
                </button>
              );
            })}
          </nav>

          <AgentDetail
            key={kind}
            kind={kind}
            section={section}
            status={agents.find((entry) => entry.kind === kind)}
            limits={limits[kind]}
            setup={setup}
            onOpenTerminal={terminal.open}
            starting={terminal.starting}
          />
        </div>
      </div>
      {terminal.picker}
    </>
  );
}

/** The agent's plan limits: what is left, when it resets, and where the pace lands. */
function PlanLimits({ kind, limits }: { kind: AgentKind; limits: Limits }) {
  const now = useMinute();
  return (
    <section className="ag-section" aria-label="Plan limits">
      <header className="ag-section-head">
        <div>
          <h3>Plan limits</h3>
          <span>Live windows with pace forecast since each window opened</span>
        </div>
        {limits.plan && <span className="pill">{limits.plan}</span>}
      </header>
      <div className="ag-limit-tiles">
        {limits.windows.map((window) => {
          const forecast = forecastLimit(window, now);
          const name = `${AGENT_NAMES[kind]} · ${shortLimitLabel(window.label)}`;
          return (
            <div key={window.label} className="ag-tile" data-tone={forecast.tone} role="group" aria-label={`${name}: ${LEFT(forecast)}% left. ${forecast.text}`}>
              <div className="ag-tile-top">
                <span className="ag-tile-name">{shortLimitLabel(window.label)}</span>
                {forecast.resetsIn != null && <span className="ag-tile-reset">Resets in {formatSpan(forecast.resetsIn)}</span>}
              </div>
              <div className="ag-tile-figure">
                <strong>{LEFT(forecast)}<small>%</small></strong>
                <span>remaining</span>
              </div>
              {burnable(window, now)
                ? <BurnChart window={window} forecast={forecast} scope={limits.scope} now={now} height={64} axis />
                : <PaceMeter forecast={forecast} label={name} />}
              <span className="ag-tile-forecast">{forecast.text}</span>
            </div>
          );
        })}
      </div>
    </section>
  );
}

/** Focused control center for one agent: hero bar, live quotas, account & runtime. */
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
  const canOperate = Boolean(status?.cliChecked && status.installed && status.version);
  const needsInstall = Boolean(status && (!status.installed || (status.cliChecked && !status.version)));
  const account = currentClaudeAccount(status);
  const provider = account?.provider || PROVIDER_NAMES[kind];
  const tab = section ?? defaultSection(status);
  const hasUnifiedLimits = Boolean(limits && limits.windows.length > 0);

  return (
    <div className="ag-workspace view-enter">
      <header className="ag-hero">
        <span className="ag-hero-mark"><AgentLogo agent={kind} size={28} /></span>
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-2">
            <h2>{AGENT_NAMES[kind]}</h2>
            <span className={`pill ${status ? PILL_TONE[state.tone] : ""}`}>{status ? state.label : "Checking…"}</span>
            {limits?.plan && <span className="pill">{limits.plan}</span>}
          </div>
          <p>
            <span>{provider}</span>
            {status?.version && <span className="tabular-nums"> · v{status.version}</span>}
            {status?.package && <> · <code className="mono text-xs text-muted">{status.package}</code></>}
          </p>
        </div>
        <div className="ag-hero-actions">
          {canOperate && state.tone === "warn" && (
            <Button size="xs" leftSection={<IconTerminal2 size={14} />} loading={starting === kind}
              disabled={starting != null} onClick={() => onOpenTerminal(kind)}>Sign in</Button>
          )}
          {canOperate && state.tone !== "warn" && (
            <Tooltip label={`Open ${AGENT_NAMES[kind]} CLI terminal`} openDelay={300}>
              <Button size="xs" variant="default" leftSection={<IconTerminal2 size={14} />} loading={starting === kind}
                disabled={starting != null} onClick={() => onOpenTerminal(kind)}>Terminal</Button>
            </Tooltip>
          )}
          {canOperate && (
            <Button size="xs" variant="default" loading={setup.installing === kind}
              disabled={setup.installing != null || nodeBlocked} onClick={() => status && void setup.install(status)}>Update CLI</Button>
          )}
          {needsInstall && (
            <Button size="xs" leftSection={<IconDownload size={14} />}
              loading={setup.installing === kind} disabled={setup.installing != null || nodeBlocked}
              onClick={() => status && void setup.install(status)}>{status?.installed ? "Reinstall" : "Install"}</Button>
          )}
          {canOperate && (
            <>
              <Button size="xs" variant="subtle" color="gray" leftSection={<IconPlug size={14} />} onClick={() => uiActions.navigate({ kind: "mcp", agent: kind })}>MCP</Button>
              <Button size="xs" variant="subtle" color="gray" leftSection={<IconSparkles size={14} />} onClick={() => uiActions.navigate({ kind: "skills", agent: kind })}>Skills</Button>
            </>
          )}
        </div>
      </header>

      {hasUnifiedLimits && limits && <PlanLimits kind={kind} limits={limits} />}

      <section className="ag-panel">
        <div className="ag-panel-head">
          <SegmentedControl
            size="xs"
            value={tab}
            onChange={(value) => show(kind, value as AgentSection)}
            aria-label={`${AGENT_NAMES[kind]} sections`}
            data={AGENT_SECTIONS.map((item) => ({
              value: item.value,
              label: item.value === "setup" ? "Runtime & Extensions" : item.label,
            }))}
          />
          {status?.configError && <span className="text-xs text-bad [overflow-wrap:anywhere]">{status.configError}</span>}
        </div>
        <div className="ag-panel-body">
          {tab === "account"
            ? <AgentAccount kind={kind} status={status} onOpenTerminal={onOpenTerminal} starting={starting} hideLimits={hasUnifiedLimits} />
            : <AgentSetupDetails kind={kind} status={status} setup={setup} />}
        </div>
      </section>
    </div>
  );
}

function AgentSetupDetails({ kind, status, setup }: { kind: AgentKind; status?: AgentStatus; setup: Setup }) {
  const runtime = setup.runtimes.find((entry) => entry.agent === kind);
  return (
    <section aria-label={`${AGENT_NAMES[kind]} setup`} className="grid min-w-0 gap-5">
      {!status ? <Loader size="xs" aria-label="Checking installation" /> : <>
        {runtime && (
          <div className="min-w-0">
            <h3 className="m-0 mb-2 text-md font-semibold">Chat runtime</h3>
            <AcpRuntimePanel runtime={runtime} agent={status} onChange={setup.loadRuntimes} />
          </div>
        )}
        {status.installed && status.version && (
          <div className="grid gap-2.5 border-t border-line/70 pt-4">
            <div className="flex flex-wrap items-center justify-between gap-2">
              <div>
                <h3 className="m-0 text-md font-semibold">Extensions &amp; backups</h3>
                <p className="m-0 mt-0.5 text-xs text-muted">Manage user-level MCP servers, shared skills, or inspect project-local skills.</p>
              </div>
              <div className="flex gap-2">
                <Button size="xs" variant="default" leftSection={<IconPlug size={14} />} onClick={() => uiActions.navigate({ kind: "mcp", agent: kind })}>MCP servers</Button>
                <Button size="xs" variant="default" leftSection={<IconSparkles size={14} />} onClick={() => uiActions.navigate({ kind: "skills", agent: kind })}>Skills</Button>
              </div>
            </div>
            <details className="extension-project-details">
              <summary className="cursor-pointer text-sm text-muted">Project skills and agent backups</summary>
              <AgentExtensionsPanel kind={kind} />
            </details>
          </div>
        )}
        {!runtime && !(status.installed && status.version) && (
          <p className="m-0 text-sm text-muted">Install {AGENT_NAMES[kind]} to configure its chat runtime and extensions.</p>
        )}
      </>}
    </section>
  );
}
