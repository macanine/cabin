import { ActionIcon, Alert, Button, Loader, Menu, Tooltip } from "@mantine/core";
import {
  IconAlertTriangle,
  IconArrowUpRight,
  IconChevronRight,
  IconCreditCard,
  IconDots,
  IconDownload,
  IconLayoutGrid,
  IconPlug,
  IconRefresh,
  IconRotateClockwise,
  IconSparkles,
  IconTerminal2,
} from "@tabler/icons-react";
import { useCallback, useRef, useState } from "react";
import { AcpRuntimePanel, runtimeSummary } from "../components/AcpRuntimePanel";
import { AGENT_KINDS } from "../components/AgentPicker";
import { AgentLogo, AGENT_NAMES } from "../components/common";
import { AgentExtensionsPanel } from "../components/extensions/AgentExtensionsPanel";
import { MachineGate } from "../components/MachineGate";
import { PaceMeter } from "../components/PaceMeter";
import { QuotaPacingCard } from "../components/QuotaPacingCard";
import { QuotaTimeline } from "../components/QuotaTimeline";
import { ViewHeader } from "../components/TitleBar";
import { forecastLimit, formatSpan } from "../lib/agentLimits";
import {
  agentState,
  attentionCount,
  currentClaudeAccount,
  needsCliLogin,
  nodeProblem,
  type AgentSection,
  type AgentState,
} from "../lib/agentsHub";
import { attempt, runOperation } from "../lib/operations";
import { formatClockTime, formatDate, formatGuestDateTime } from "../lib/time";
import { useAutoRefresh } from "../lib/useAutoRefresh";
import { useMinute } from "../lib/useNow";
import { agentsIpc } from "../services/ipc/agents";
import { appIpc } from "../services/ipc/app";
import type { AcpRuntime, AgentKind, AgentStatus, NodeState } from "../services/ipc/generated/core";
import { nodeIpc } from "../services/ipc/node";
import { useUnifiedQuotas, type AgentQuotaSummary, type UnifiedQuotaWindow } from "../state/quotaStore";
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
      })
      .finally(() => {
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
      await runOperation(
        appIpc.runSetup({ kind: "installAgent", agent: agent.kind }),
        `${AGENT_NAMES[agent.kind]} ${action}`,
      );
    } finally {
      setInstalling(null);
    }
  }

  return { problem: nodeProblem(node), runtimes, loadRuntimes, installing, install, checking, checkError, check };
}
type Setup = ReturnType<typeof useAgentSetup>;

type OpenAgentTerminal = (kind: AgentKind, login: boolean) => void;

function useAgentTerminal() {
  const [starting, setStarting] = useState<AgentKind | null>(null);
  const pending = useRef(false);

  async function open(kind: AgentKind, login: boolean) {
    if (pending.current) return;
    pending.current = true;
    const from = uiActions.getView();
    setStarting(kind);
    try {
      const session = await attempt(agentsIpc.openTerminal(kind, login));
      if (session && uiActions.getView() === from) uiActions.navigate({ kind: "session", id: session.id });
    } finally {
      pending.current = false;
      setStarting(null);
    }
  }

  return { starting, open };
}

export function AgentsView({ agent, section }: { agent?: AgentKind; section?: AgentSection }) {
  return <MachineGate><AgentsContent selected={agent} section={section} /></MachineGate>;
}

const showOverview = () => uiActions.navigate({ kind: "agents" });
const showAgent = (agent: AgentKind, section?: AgentSection) => uiActions.navigate({ kind: "agents", agent, section });

const DOT: Record<AgentState["tone"], string> = {
  ok: "bg-ok", info: "bg-info", warn: "bg-warn", bad: "bg-bad", off: "bg-faint/45",
};

function tightestWindow(windows: readonly UnifiedQuotaWindow[], now: number): { left: number; tone: "ok" | "warn" | "bad" } | null {
  if (windows.length === 0) return null;
  let worst: { left: number; tone: "ok" | "warn" | "bad" } | null = null;
  for (const w of windows) {
    const f = forecastLimit(w, now);
    const left = Math.round(f.left);
    if (!worst || left < worst.left || (f.tone === "bad" && worst.tone !== "bad")) {
      worst = { left, tone: f.tone };
    }
  }
  return worst;
}

/**
 * Unified Agents & Quotas Workspace:
 * - Top pill rail: `Overview` (unified 5h/7d reset timeline, pacing curves & all-agent directory)
 *   plus individual agent pills with inline live remaining % badges.
 * - Level 1 (`Overview`): Magpie Next 5h / Next 7d reset timeline -> unified pacing cards -> 5-agent directory.
 * - Level 2 (Single Agent): Hero header -> unified pacing & entitlements -> clean account panel -> runtime & extensions.
 */
function AgentsContent({ selected, section }: { selected?: AgentKind; section?: AgentSection }) {
  const agents = useApp((state) => state.agents);
  const setup = useAgentSetup();
  const terminal = useAgentTerminal();
  const quotas = useUnifiedQuotas(true);
  const now = useMinute();
  const { t } = useI18n();

  const installed = agents.filter((agent) => agent.installed).length;
  const attention = attentionCount(agents);
  const activeTab: "overview" | AgentKind = selected ?? "overview";

  const refreshEverything = () => {
    void setup.check();
    void quotas.refreshAll(true);
  };

  return (
    <>
      <ViewHeader
        title={t("agents.title")}
        subtitle={agents.length > 0 ? (
          <>
            {t("home.agentsSummary", { ready: installed, total: AGENT_KINDS.length })}
            {attention > 0 && <span className="text-warn"> · {t("home.needAttention", { count: attention })}</span>}
            {quotas.refreshedAt != null && <span className="text-faint"> · {t("quota.checkedAt", { time: formatGuestDateTime(quotas.refreshedAt) })}</span>}
          </>
        ) : t("agents.subtitle")}
      >
        <Tooltip label={t("quota.refreshAll")} openDelay={300}>
          <ActionIcon
            size="md"
            variant="subtle"
            color="gray"
            aria-label={t("quota.refreshAll")}
            disabled={setup.installing != null}
            loading={setup.checking || quotas.anyLoading}
            onClick={refreshEverything}
          >
            <IconRefresh size={16} />
          </ActionIcon>
        </Tooltip>
      </ViewHeader>

      <div className="ag-page">
        <div className={`ag-shell${activeTab === "overview" ? " wide" : ""}`}>
          <nav className="ag-bar" aria-label={t("agents.title")}>
            <button
              type="button"
              className="ag-pill press"
              aria-current={activeTab === "overview" ? "page" : undefined}
              onClick={showOverview}
            >
              <IconLayoutGrid size={15} className="shrink-0" />
              <span>{t("quota.overview")}</span>
            </button>
            <span className="ag-bar-sep" aria-hidden />
            {AGENT_KINDS.map((entry) => {
              const status = agents.find((agent) => agent.kind === entry);
              const state = agentState(status);
              const active = entry === activeTab;
              const badge = tightestWindow(quotas.byAgent[entry].windows, now);
              return (
                <button
                  key={entry}
                  type="button"
                  className="ag-pill press"
                  aria-current={active ? "page" : undefined}
                  data-installed={status?.installed ? "true" : "false"}
                  onClick={() => showAgent(entry)}
                >
                  <AgentLogo agent={entry} size={16} />
                  <span>{AGENT_NAMES[entry]}</span>
                  {badge ? (
                    <span className="ag-pill-quota" data-tone={badge.tone}>{badge.left}%</span>
                  ) : (
                    <span
                      className={`ag-pill-dot ${status ? DOT[state.tone] : "bg-faint/40"}`}
                      title={status ? state.label : t("common.loading")}
                      aria-hidden
                    />
                  )}
                </button>
              );
            })}
          </nav>

          {setup.checkError && <p className="callout warn m-0" role="status">{t("agents.checkError")}</p>}
          {setup.problem && (
            <div className="callout warn flex flex-wrap items-center justify-between gap-2">
              <span>{setup.problem}</span>
              <Button size="xs" variant="default" onClick={() => uiActions.navigate({ kind: "settings", tab: "tools" })}>
                {t("agents.setupNode")}
              </Button>
            </div>
          )}

          {activeTab === "overview" ? (
            <AgentsOverview
              agents={agents}
              quotas={quotas}
              now={now}
              setup={setup}
              onOpenTerminal={terminal.open}
              starting={terminal.starting}
            />
          ) : (
            <AgentCanvas
              key={activeTab}
              kind={activeTab}
              openAccount={section === "account"}
              openSetup={section === "setup"}
              status={agents.find((entry) => entry.kind === activeTab)}
              quota={quotas.byAgent[activeTab]}
              onRefreshQuota={() => void quotas.refreshAgent(activeTab, true)}
              setup={setup}
              onOpenTerminal={terminal.open}
              starting={terminal.starting}
            />
          )}
        </div>
      </div>
    </>
  );
}

// ------------------------------------------------------------------ Overview

function AgentsOverview({
  agents,
  quotas,
  now,
  setup,
  onOpenTerminal,
  starting,
}: {
  agents: readonly AgentStatus[];
  quotas: ReturnType<typeof useUnifiedQuotas>;
  now: number;
  setup: Setup;
  onOpenTerminal: OpenAgentTerminal;
  starting: AgentKind | null;
}) {
  const { t } = useI18n();
  // Sort 5h windows before 7d windows, then by soonest reset.
  const orderedWindows = quotas.allWindows.slice().sort((a, b) => {
    const rank = (tier: UnifiedQuotaWindow["tier"]) => (tier === "5h" ? 0 : tier === "7d" ? 1 : 2);
    if (rank(a.tier) !== rank(b.tier)) return rank(a.tier) - rank(b.tier);
    return (a.resetsAt ?? Number.MAX_SAFE_INTEGER) - (b.resetsAt ?? Number.MAX_SAFE_INTEGER);
  });

  return (
    <div className="ag-overview view-enter">
      {/* 1. Magpie Next 5h / Next 7d Reset Timeline */}
      <section className="ag-block" aria-label={t("quota.timelineHeading")}>
        <div className="ag-block-head">
          <div>
            <h3>{t("quota.timelineHeading")}</h3>
            <span>{t("quota.timelineSubtitle")}</span>
          </div>
          <Button
            size="compact-xs"
            variant="subtle"
            color="gray"
            leftSection={<IconRefresh size={13} />}
            loading={quotas.anyLoading}
            onClick={() => void quotas.refreshAll(true)}
          >
            {t("common.refresh")}
          </Button>
        </div>
        <QuotaTimeline
          windows={orderedWindows}
          byAgent={quotas.byAgent}
          now={now}
          onSelectAgent={(kind) => showAgent(kind)}
        />
      </section>

      {/* 2. Unified Pacing Curves across all active 5h / 7d windows */}
      {orderedWindows.length > 0 && (
        <section className="ag-block" aria-label={t("quota.pacingHeading")}>
          <div className="ag-block-head">
            <div>
              <h3>{t("quota.pacingHeading")}</h3>
              <span>{t("quota.pacingSubtitle")}</span>
            </div>
            <Button
              size="compact-xs"
              variant="subtle"
              color="gray"
              rightSection={<IconArrowUpRight size={13} />}
              onClick={() => uiActions.navigate({ kind: "usage" })}
            >
              {t("quota.viewSpend")}
            </Button>
          </div>
          <div className="qp-grid">
            {orderedWindows.map((window) => (
              <QuotaPacingCard
                key={window.id}
                window={window}
                now={now}
                showAgent
                onSelect={() => showAgent(window.agent)}
              />
            ))}
          </div>
        </section>
      )}

      {/* 3. All 5 Agents Directory */}
      <section className="ag-block" aria-label={t("quota.directoryHeading")}>
        <div className="ag-block-head">
          <h3>{t("quota.directoryHeading")}</h3>
          <span>{t("quota.directoryHint")}</span>
        </div>
        <div className="ag-directory">
          {AGENT_KINDS.map((kind) => {
            const status = agents.find((entry) => entry.kind === kind);
            const state = agentState(status);
            const summary = quotas.byAgent[kind];
            const installed = Boolean(status?.installed && (!status.cliChecked || status.version));
            const needsInstall = Boolean(status && (!status.installed || (status.cliChecked && !status.version)));
            const nodeBlocked = (status?.requiresNode ?? true) && setup.problem != null;
            const login = status != null && needsCliLogin(status);

            return (
              <div key={kind} className="ag-dir-card" data-installed={installed ? "true" : "false"}>
                <button type="button" className="ag-dir-top press" onClick={() => showAgent(kind)}>
                  <span className="ag-dir-logo"><AgentLogo agent={kind} size={20} /></span>
                  <div className="min-w-0 flex-1">
                    <div className="flex items-center gap-1.5">
                      <strong>{AGENT_NAMES[kind]}</strong>
                      {summary.plan && <span className="ag-dir-plan">{summary.plan}</span>}
                    </div>
                    <span className="ag-dir-sub">
                      <span className={`ag-pill-dot ${status ? DOT[state.tone] : "bg-faint/40"}`} aria-hidden />
                      <span>{status ? state.label : t("common.loading")}</span>
                      {status?.version && <span>· v{status.version}</span>}
                    </span>
                  </div>
                  <IconChevronRight size={15} className="shrink-0 text-faint" />
                </button>

                <div className="ag-dir-body">
                  {summary.windows.length > 0 ? (
                    <div className="ag-dir-windows">
                      {summary.windows.slice(0, 3).map((window) => {
                        const forecast = forecastLimit(window, now);
                        const left = Math.round(forecast.left);
                        return (
                          <div key={window.id} className="ag-dir-win">
                            <div className="ag-dir-win-head">
                              <span className="ag-dir-win-label">{window.shortLabel}</span>
                              <span className="ag-dir-win-meta">
                                <strong>{left}%</strong>
                                {forecast.resetsIn != null && (
                                  <span> · {formatSpan(forecast.resetsIn)}</span>
                                )}
                              </span>
                            </div>
                            <PaceMeter forecast={forecast} size="sm" label={`${AGENT_NAMES[kind]} ${window.shortLabel}`} />
                          </div>
                        );
                      })}
                    </div>
                  ) : summary.loading ? (
                    <span className="ag-dir-note"><Loader size={12} /> {t("quota.readingLimits")}</span>
                  ) : summary.error ? (
                    <span className="ag-dir-note warn">{summary.error}</span>
                  ) : (
                    <span className="ag-dir-note">
                      {!installed
                        ? AGENT_BLURBS[kind]
                        : kind === "claude" || kind === "codex"
                          ? t("quota.signInForLimits")
                          : t("quota.noRollingWindow")}
                    </span>
                  )}
                </div>

                <div className="ag-dir-foot">
                  {summary.subscriptionUntil != null && (
                    <span className="ag-dir-expiry">
                      {t("quota.extra.paidThrough", { date: formatDate(summary.subscriptionUntil) })}
                    </span>
                  )}
                  <div className="ag-dir-actions">
                    {needsInstall && status ? (
                      <Button
                        size="compact-xs"
                        leftSection={<IconDownload size={12} />}
                        loading={setup.installing === kind}
                        disabled={setup.installing != null || nodeBlocked}
                        onClick={() => void setup.install(status)}
                      >
                        {t("common.install")}
                      </Button>
                    ) : installed && status?.cliChecked ? (
                      <Button
                        size="compact-xs"
                        variant={login ? "filled" : "default"}
                        leftSection={<IconTerminal2 size={12} />}
                        loading={starting === kind}
                        disabled={starting != null}
                        onClick={() => onOpenTerminal(kind, login)}
                      >
                        {t(login ? "agents.signIn" : "agents.openTerminal")}
                      </Button>
                    ) : null}
                  </div>
                </div>
              </div>
            );
          })}
        </div>
      </section>
    </div>
  );
}

// ------------------------------------------------------------ Single Agent

function AgentQuotaSection({
  kind,
  quota,
  onRefresh,
}: {
  kind: AgentKind;
  quota: AgentQuotaSummary;
  onRefresh: () => void;
}) {
  const { t } = useI18n();
  const now = useMinute();
  const supportsQuota = kind === "claude" || kind === "codex";
  const hasEntitlements =
    quota.subscriptionUntil != null ||
    Boolean(quota.credits && (quota.credits.unlimited || quota.credits.hasCredits)) ||
    quota.resetCreditCount > 0;

  return (
    <section className="ag-block" aria-label={t("agents.planLimits")}>
      <div className="ag-block-head">
        <div>
          <h3>{t("agents.planLimits")}</h3>
          <span>
            {quota.refreshedAt != null
              ? t("quota.checkedAt", { time: formatGuestDateTime(quota.refreshedAt) })
              : t("agents.paceHint")}
          </span>
        </div>
        {supportsQuota && (
          <Button
            size="compact-xs"
            variant="subtle"
            color="gray"
            leftSection={<IconRefresh size={13} />}
            loading={quota.loading}
            onClick={onRefresh}
          >
            {t("common.refresh")}
          </Button>
        )}
      </div>

      {quota.restriction && (
        <Alert color="yellow" icon={<IconAlertTriangle size={15} />}>
          {quota.restriction}
        </Alert>
      )}
      {quota.error && (
        <Alert color="yellow" icon={<IconAlertTriangle size={15} />}>
          {quota.error}
        </Alert>
      )}

      {quota.windows.length > 0 ? (
        <div className="qp-grid">
          {quota.windows.map((window) => (
            <QuotaPacingCard key={window.id} window={window} now={now} />
          ))}
        </div>
      ) : quota.loading ? (
        <div className="ag-quota-empty">
          <Loader size="xs" />
          <span>{t("quota.readingLimits")}</span>
        </div>
      ) : (
        <div className="ag-quota-empty">
          <span>
            {supportsQuota
              ? quota.noLimits
                ? t("quota.noPlanWindows")
                : t("quota.signInForLimits")
              : t("quota.noRollingWindow")}
          </span>
          <Button
            size="compact-xs"
            variant="subtle"
            rightSection={<IconArrowUpRight size={13} />}
            onClick={() => uiActions.navigate({ kind: "usage" })}
          >
            {t("quota.viewSpend")}
          </Button>
        </div>
      )}

      {hasEntitlements && (
        <div className="ag-entitlements">
          {quota.subscriptionUntil != null && (
            <div className="ag-entitlement">
              <IconSparkles size={14} className="text-muted" />
              <span>{t("quota.extra.subscriptionShort")}</span>
              <strong>{t("quota.extra.paidThrough", { date: formatDate(quota.subscriptionUntil) })}</strong>
            </div>
          )}
          {quota.credits && (quota.credits.unlimited || quota.credits.hasCredits) && (
            <div className="ag-entitlement">
              <IconCreditCard size={14} className="text-muted" />
              <span>{t("quota.extra.creditsShort")}</span>
              <strong>
                {quota.credits.unlimited
                  ? t("quota.extra.unlimited")
                  : (quota.credits.balance ?? t("quota.extra.available"))}
              </strong>
            </div>
          )}
          {quota.resetCreditCount > 0 && (
            <div className="ag-entitlement">
              <IconRotateClockwise size={14} className="text-muted" />
              <span>{t("quota.extra.resetCards", { count: quota.resetCreditCount })}</span>
              {quota.resetCards[0]?.expiresAt != null && (
                <strong>
                  {t("quota.extra.expiresOn", {
                    date: formatClockTime(quota.resetCards[0].expiresAt, now),
                  })}
                </strong>
              )}
            </div>
          )}
        </div>
      )}
    </section>
  );
}

function AgentCanvas({
  kind,
  openAccount,
  openSetup,
  status,
  quota,
  onRefreshQuota,
  setup,
  onOpenTerminal,
  starting,
}: {
  kind: AgentKind;
  openAccount: boolean;
  openSetup: boolean;
  status?: AgentStatus;
  quota: AgentQuotaSummary;
  onRefreshQuota: () => void;
  setup: Setup;
  onOpenTerminal: OpenAgentTerminal;
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
  const [runtimeOpen, setRuntimeOpen] = useState(openSetup);

  if (!status) {
    return (
      <div className="ag-empty">
        <Loader size="xs" aria-label={t("agents.checking", { agent: AGENT_NAMES[kind] })} />
        <span>{t("agents.checking", { agent: AGENT_NAMES[kind] })}</span>
      </div>
    );
  }

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
          {status.installed ? t("agents.reinstallBtn", { agent: AGENT_NAMES[kind] }) : t("agents.installBtn", { agent: AGENT_NAMES[kind] })}
        </Button>
      </section>
    );
  }

  const metaParts = [
    [provider, quota.plan].filter(Boolean).join(" "),
    status.version ? `v${status.version}` : null,
    quota.accountLabel ?? account?.email ?? null,
    state.tone !== "ok" ? state.label : null,
  ].filter(Boolean);
  const login = needsCliLogin(status);
  const accountPanel = (
    <section className="ag-block" aria-label={t("agents.account")}>
      <div className="ag-block-head"><h3>{t("agents.account")}</h3></div>
      <div className="ag-surface">
        <AgentAccount kind={kind} status={status} onOpenTerminal={onOpenTerminal} starting={starting} hideLimits />
      </div>
    </section>
  );
  const accountFirst = openAccount || (login && !openSetup);

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
          {canOperate && (
            <Button
              size="xs"
              variant={login ? "filled" : "default"}
              leftSection={<IconTerminal2 size={14} />}
              loading={starting === kind}
              disabled={starting != null}
              onClick={() => onOpenTerminal(kind, login)}
            >
              {t(login ? "agents.signIn" : "agents.openTerminal")}
            </Button>
          )}
          <Menu position="bottom-end" withinPortal shadow="md" width={196}>
            <Menu.Target>
              <ActionIcon variant="subtle" color="gray" size="md" aria-label={`More actions for ${AGENT_NAMES[kind]}`}>
                <IconDots size={16} />
              </ActionIcon>
            </Menu.Target>
            <Menu.Dropdown>
              {canOperate && (
                <Menu.Item
                  leftSection={<IconDownload size={14} />}
                  disabled={setup.installing != null || nodeBlocked}
                  onClick={() => void setup.install(status)}
                >
                  {t("agents.updateCli")}
                </Menu.Item>
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

      {accountFirst && accountPanel}
      <AgentQuotaSection kind={kind} quota={quota} onRefresh={onRefreshQuota} />
      {!accountFirst && accountPanel}

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
