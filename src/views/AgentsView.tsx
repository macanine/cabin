import { ActionIcon, Button, Loader, Menu, Tooltip } from "@mantine/core";
import {
  IconChevronRight, IconDots, IconDownload, IconPlug, IconRefresh, IconSparkles, IconTerminal2,
} from "@tabler/icons-react";
import { useCallback, useRef, useState } from "react";
import { AcpRuntimePanel, runtimeSummary } from "../components/AcpRuntimePanel";
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
import { agentState, attentionCount, currentClaudeAccount, nodeProblem, type AgentSection, type AgentState } from "../lib/agentsHub";
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
import { useI18n } from "../i18n";

const AGENT_BLURBS: Record<AgentKind, string> = {
  claude: "Anthropic’s coding agent CLI with Claude.ai subscription or API billing.",
  codex: "OpenAI’s coding agent CLI with multi-account switching and quota tracking.",
  opencode: "Open-source multi-provider coding agent running via native ACP.",
  pi: "Lightweight extensible coding agent CLI with custom provider support.",
  antigravity: "Google’s agentic coding CLI with live model and thinking controls.",
};

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

function firstAgent(agents: AgentStatus[]): AgentKind {
  const by = (test: (agent: AgentStatus) => boolean) => AGENT_KINDS.find((kind) => agents.some((agent) => agent.kind === kind && test(agent)));
  return by((agent) => agent.installed && ["warn", "bad"].includes(agentState(agent).tone)) ?? by((agent) => agent.installed) ?? AGENT_KINDS[0];
}

const show = (agent: AgentKind, section?: AgentSection) => uiActions.navigate({ kind: "agents", agent, section });
const LEFT = (forecast: LimitForecast) => Math.max(0, Math.round(100 - forecast.used));

const DOT: Record<AgentState["tone"], string> = {
  ok: "bg-ok", info: "bg-info", warn: "bg-warn", bad: "bg-bad", off: "bg-faint/45",
};

/**
 * Calm, zero-duplication Agents workspace:
 * 1) One lightweight horizontal agent bar at the top (logo + name + status dot).
 * 2) One focused, spacious single-column canvas below where every fact appears
 *    exactly once (header with single primary action, live plan limits if any,
 *    account controls, and compact runtime/extensions disclosures).
 */
function AgentsContent({ selected, section }: { selected?: AgentKind; section?: AgentSection }) {
  const agents = useApp((state) => state.agents);
  const setup = useAgentSetup();
  const terminal = useAgentTerminal();
  const { t } = useI18n();
  const [initial, setInitial] = useState<AgentKind | null>(null);
  if (initial == null && agents.length > 0) setInitial(firstAgent(agents));
  const kind = selected ?? initial ?? AGENT_KINDS[0];
  const limits = useAgentLimits(agents, true);
  const installed = agents.filter((agent) => agent.installed).length;
  const attention = attentionCount(agents);

  return (
    <>
      <ViewHeader
        title={t("agents.title")}
        subtitle={agents.length > 0 ? (
          <>
            {t("home.agentsSummary", { ready: installed, total: AGENT_KINDS.length })}
            {attention > 0 && <span className="text-warn"> · {t("home.needAttention", { count: attention })}</span>}
          </>
        ) : t("agents.subtitle")}
      >
        <Tooltip label="Refresh agent status" openDelay={300}>
          <ActionIcon size="md" variant="subtle" color="gray" aria-label="Refresh agent status"
            disabled={setup.installing != null} loading={setup.checking} onClick={() => void setup.check()}>
            <IconRefresh size={16} />
          </ActionIcon>
        </Tooltip>
      </ViewHeader>

      <div className="ag-page">
        <div className="ag-shell">
          <nav className="ag-bar" aria-label="Agents">
            {AGENT_KINDS.map((entry) => {
              const status = agents.find((agent) => agent.kind === entry);
              const state = agentState(status);
              const active = entry === kind;
              return (
                <button
                  key={entry}
                  type="button"
                  className="ag-pill press"
                  aria-current={active ? "page" : undefined}
                  data-installed={status?.installed ? "true" : "false"}
                  onClick={() => show(entry)}
                >
                  <AgentLogo agent={entry} size={17} />
                  <span>{AGENT_NAMES[entry]}</span>
                  <span className={`ag-pill-dot ${status ? DOT[state.tone] : "bg-faint/40"}`} title={status ? state.label : "Checking…"} aria-hidden />
                </button>
              );
            })}
          </nav>

          {setup.checkError && <p className="callout warn m-0" role="status">Couldn’t check agent status. Refresh to retry.</p>}
          {setup.problem && (
            <div className="callout warn flex flex-wrap items-center justify-between gap-2">
              <span>{setup.problem}</span>
              <Button size="xs" variant="default" onClick={() => uiActions.navigate({ kind: "settings", tab: "tools" })}>Set up Node.js</Button>
            </div>
          )}

          <AgentCanvas
            key={kind}
            kind={kind}
            openSetup={section === "setup"}
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

function PlanLimits({ kind, limits }: { kind: AgentKind; limits: Limits }) {
  const now = useMinute();
  return (
    <section className="ag-block" aria-label="Plan limits">
      <div className="ag-block-head">
        <h3>Plan limits</h3>
        <span>Pace estimate since each window opened</span>
      </div>
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
                <span>left</span>
              </div>
              {burnable(window, now)
                ? <BurnChart window={window} forecast={forecast} scope={limits.scope} now={now} height={60} axis />
                : <PaceMeter forecast={forecast} label={name} />}
              <span className="ag-tile-forecast">{forecast.text}</span>
            </div>
          );
        })}
      </div>
    </section>
  );
}

function AgentCanvas({ kind, openSetup, status, limits, setup, onOpenTerminal, starting }: {
  kind: AgentKind;
  openSetup: boolean;
  status?: AgentStatus;
  limits?: Limits | null;
  setup: Setup;
  onOpenTerminal: (kind: AgentKind) => void;
  starting: AgentKind | null;
}) {
  const { t } = useI18n();
  const state = agentState(status);
  const nodeBlocked = (status?.requiresNode ?? true) && setup.problem != null;
  const canOperate = Boolean(status?.cliChecked && status.installed && status.version);
  const needsInstall = Boolean(status && (!status.installed || (status.cliChecked && !status.version)));
  const account = currentClaudeAccount(status);
  const provider = account?.provider || PROVIDER_NAMES[kind];
  const runtime = setup.runtimes.find((entry) => entry.agent === kind);
  const hasLimits = Boolean(limits && limits.windows.length > 0);
  const [runtimeOpen, setRuntimeOpen] = useState(openSetup);

  if (!status) {
    return (
      <div className="ag-empty">
        <Loader size="xs" aria-label="Checking agent" />
        <span>Checking {AGENT_NAMES[kind]}…</span>
      </div>
    );
  }

  // Uninstalled or broken CLI: calm, single-focus install card (no empty tabs).
  if (needsInstall) {
    return (
      <section className="ag-install-card view-enter">
        <span className="ag-install-logo"><AgentLogo agent={kind} size={32} /></span>
        <h2>{AGENT_NAMES[kind]}</h2>
        <p>{AGENT_BLURBS[kind]}</p>
        <div className="ag-install-meta">
          <span>{provider}</span>
          <span>·</span>
          <code>{status.package}</code>
        </div>
        {status.configError && <p className="m-0 text-xs text-bad [overflow-wrap:anywhere]">{status.configError}</p>}
        <Button
          size="sm"
          mt="xs"
          leftSection={<IconDownload size={15} />}
          loading={setup.installing === kind}
          disabled={setup.installing != null || nodeBlocked}
          onClick={() => void setup.install(status)}
        >
          {status.installed ? `Reinstall ${AGENT_NAMES[kind]}` : `Install ${AGENT_NAMES[kind]}`}
        </Button>
      </section>
    );
  }

  const metaParts = [
    [provider, limits?.plan].filter(Boolean).join(" "),
    status.version ? `v${status.version}` : null,
    account?.email ?? null,
    state.tone !== "ok" ? state.label : null,
  ].filter(Boolean);

  return (
    <div className="ag-canvas view-enter">
      <header className="ag-header">
        <span className="ag-header-logo"><AgentLogo agent={kind} size={26} /></span>
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2">
            <h2>{AGENT_NAMES[kind]}</h2>
            <span className={`size-2 shrink-0 rounded-full ${DOT[state.tone]}`} title={state.label} aria-hidden />
          </div>
          <p className="ag-header-meta" title={metaParts.join(" · ")}>{metaParts.join(" · ")}</p>
        </div>
        <div className="flex shrink-0 items-center gap-1.5">
          {canOperate && state.tone === "warn" ? (
            <Button size="xs" leftSection={<IconTerminal2 size={14} />} loading={starting === kind}
              disabled={starting != null} onClick={() => onOpenTerminal(kind)}>{t("agents.signIn")}</Button>
          ) : canOperate ? (
            <Button size="xs" variant="default" leftSection={<IconTerminal2 size={14} />} loading={starting === kind}
              disabled={starting != null} onClick={() => onOpenTerminal(kind)}>{t("agents.openTerminal")}</Button>
          ) : null}
          <Menu position="bottom-end" withinPortal shadow="md" width={196}>
            <Menu.Target>
              <ActionIcon variant="subtle" color="gray" size="md" aria-label={`More actions for ${AGENT_NAMES[kind]}`}>
                <IconDots size={16} />
              </ActionIcon>
            </Menu.Target>
            <Menu.Dropdown>
              {canOperate && (
                <Menu.Item leftSection={<IconDownload size={14} />} disabled={setup.installing != null || nodeBlocked}
                  onClick={() => void setup.install(status)}>{t("agents.updateCli")}</Menu.Item>
              )}
              <Menu.Item leftSection={<IconPlug size={14} />} onClick={() => uiActions.navigate({ kind: "mcp", agent: kind })}>
                {t("agents.mcpServers")}
              </Menu.Item>
              <Menu.Item leftSection={<IconSparkles size={14} />} onClick={() => uiActions.navigate({ kind: "skills", agent: kind })}>
                {t("agents.skills")}
              </Menu.Item>
            </Menu.Dropdown>
          </Menu>
        </div>
      </header>

      {status.configError && <p className="callout bad m-0" role="alert">{status.configError}</p>}

      {hasLimits && limits && <PlanLimits kind={kind} limits={limits} />}

      <section className="ag-block" aria-label={t("agents.account")}>
        <div className="ag-block-head">
          <h3>{t("agents.account")}</h3>
        </div>
        <div className="ag-surface">
          <AgentAccount kind={kind} status={status} onOpenTerminal={onOpenTerminal} starting={starting} hideLimits={hasLimits} />
        </div>
      </section>

      <section className="ag-block" aria-label={t("agents.runtimeAndExtensions")}>
        <div className="ag-block-head">
          <h3>{t("agents.runtimeAndExtensions")}</h3>
        </div>
        <div className="ag-links-card">
          {runtime && (
            <div className="ag-link-group">
              <button type="button" className="ag-link-row press" aria-expanded={runtimeOpen} onClick={() => setRuntimeOpen((v) => !v)}>
                <div className="min-w-0 flex-1">
                  <strong>{t("agents.chatRuntime")}</strong>
                  <span>{runtimeSummary(runtime) ?? `${runtime.selected} · ${runtime.package}`}</span>
                </div>
                <IconChevronRight size={15} className={`text-faint transition-transform duration-150 ${runtimeOpen ? "rotate-90" : ""}`} />
              </button>
              {runtimeOpen && (
                <div className="ag-link-expand">
                  <AcpRuntimePanel runtime={runtime} agent={status} onChange={setup.loadRuntimes} />
                </div>
              )}
            </div>
          )}
          <button type="button" className="ag-link-row press" onClick={() => uiActions.navigate({ kind: "mcp", agent: kind })}>
            <IconPlug size={16} className="shrink-0 text-muted" />
            <div className="min-w-0 flex-1">
              <strong>{t("agents.mcpServers")}</strong>
              <span>{t("agents.mcpHint", { agent: AGENT_NAMES[kind] })}</span>
            </div>
            <IconChevronRight size={15} className="text-faint" />
          </button>
          <button type="button" className="ag-link-row press" onClick={() => uiActions.navigate({ kind: "skills", agent: kind })}>
            <IconSparkles size={16} className="shrink-0 text-muted" />
            <div className="min-w-0 flex-1">
              <strong>{t("agents.skills")}</strong>
              <span>{t("agents.skillsHint", { agent: AGENT_NAMES[kind] })}</span>
            </div>
            <IconChevronRight size={15} className="text-faint" />
          </button>
          <details className="ag-backups-details">
            <summary className="ag-link-row press">
              <div className="min-w-0 flex-1">
                <strong>{t("agents.projectSkillsBackups")}</strong>
                <span>{t("agents.projectSkillsBackupsHint")}</span>
              </div>
              <IconChevronRight size={15} className="ag-backups-chevron text-faint" />
            </summary>
            <div className="ag-link-expand">
              <AgentExtensionsPanel kind={kind} />
            </div>
          </details>
        </div>
      </section>
    </div>
  );
}
