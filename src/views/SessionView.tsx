import { Button, Menu } from "@mantine/core";
import { TitleBarContent } from "../components/TitleBar";
import { IconChevronDown, IconPlayerPlay, IconPlayerPlayFilled, IconPlayerStopFilled, IconRefresh, IconStarFilled } from "@tabler/icons-react";
import { useEffect, useRef, useState } from "react";
import { libraryIpc } from "../services/ipc/library";
import { useApp } from "../state/store";
import { markSeen, useActivity } from "../state/unread";
import { attempt } from "../lib/operations";
import { AGENT_NAMES, CircleButton, SessionPill } from "../components/common";
import { SessionTerminal, type TerminalSize } from "../components/Terminal";
import { SessionMenuItems } from "../components/SessionMenu";
import { InlineRename } from "../components/Sidebar";
import { Chat } from "../components/Chat";
import { PanelToggles, WorkbenchPanel } from "../components/workbench/WorkbenchPanel";
import { useGitStatus } from "../components/workbench/useGitStatus";
import { uiActions, useUi } from "../state/ui";
import { useI18n } from "../i18n";

const LOGIN_GUIDANCE = {
  antigravity: "agents.loginBrowser",
  claude: "agents.loginBrowser",
  codex: "agents.loginDevice",
  opencode: "agents.loginProvider",
  pi: "agents.loginPi",
} as const;

export function SessionView({ id }: { id: string }) {
  const { t } = useI18n();
  const session = useApp((state) => state.sessions.find((s) => s.id === id));
  const workspace = useApp((state) => state.workspaces.find((w) => w.id === session?.workspaceId));
  const size = useRef<TerminalSize>({ cols: 100, rows: 30 });
  const [renaming, setRenaming] = useState(false);
  useEffect(() => markSeen(id), [id]);
  const activity = useActivity().get(id);
  const panelOpen = useUi((ui) => ui.panel.open);
  const root = { workspaceId: session?.workspaceId ?? "", sessionId: session ? id : null };
  const loginAgent = session?.mode === "login" ? session.agent : null;
  const git = useGitStatus(root, panelOpen && loginAgent == null);

  if (!session || !workspace) {
    return (
      <div className="page">
        <p className="muted">This session no longer exists.</p>
      </div>
    );
  }

  const live = session.status.state === "running" || session.status.state === "starting";
  const terminal = session.agent === null;
  const chat = session.mode === "ui";
  const start = (resume: boolean) =>
    attempt(libraryIpc.startSession(session.id, resume, size.current.cols, size.current.rows));

  return (
    <>
      <TitleBarContent className="session-bar">
        <div className="grow session-title">
          <span className="crumb">{workspace.name}</span>
          <span className="faint"> / </span>
          {renaming ? (
            <InlineRename
              className="title-rename"
              value={session.name}
              onDone={(next) => {
                setRenaming(false);
                if (next) void attempt(libraryIpc.updateSession(session.id, { name: next }));
              }}
            />
          ) : (
            <Menu position="bottom-start" withinPortal shadow="md" width={200}>
              <Menu.Target>
                <button className="title-button" aria-label="Session actions">
                  {session.starred && <IconStarFilled size={12} color="var(--warn)" />}
                  <strong className="truncate">{session.name}</strong>
                  <IconChevronDown size={14} className="faint" />
                </button>
              </Menu.Target>
              <Menu.Dropdown>
                <SessionMenuItems session={session} onRename={() => setRenaming(true)} />
              </Menu.Dropdown>
            </Menu>
          )}
        </div>
        {chat && activity && (
          <span className={`inline-flex shrink-0 items-center gap-1.5 rounded-full px-2.5 py-1 text-xs font-medium ${activity === "waiting" ? "bg-warn/12 text-warn" : "bg-hover text-muted"}`} role="status">
            <span className={`size-1.5 rounded-full ${activity === "waiting" ? "bg-warn" : "live-dot bg-ok"}`} aria-hidden />
            {activity === "waiting" ? "Needs you" : "Working"}
          </span>
        )}
        {loginAgent == null && <PanelToggles git={git} />}
        {!chat && !terminal && <SessionPill status={session.status} />}
        {!chat && !terminal && (live ? (
          <CircleButton label="Stop" onClick={() => attempt(libraryIpc.stopSession(session.id))}><IconPlayerStopFilled size={13} /></CircleButton>
        ) : (
          <CircleButton label={loginAgent ? t("agents.retryLogin") : session.lastStartedAt != null ? "Continue the most recent conversation in this folder" : "Start"}
            primary onClick={() => start(loginAgent == null && session.lastStartedAt != null)}>
            <IconPlayerPlayFilled size={13} />
          </CircleButton>
        ))}
      </TitleBarContent>
      <div className="session-body">
      {chat && session.agent ? <Chat key={session.id} sessionId={session.id} workspaceId={workspace.id} agent={session.agent} /> : <div className={loginAgent ? "terminal-wrap flex-col" : "terminal-wrap"}>
        {loginAgent && (
          <div className="flex shrink-0 flex-wrap items-center gap-x-4 gap-y-2 border-b border-line bg-surface px-4 py-3">
            <div className="min-w-0 flex-1">
              <strong className="text-sm">{t("agents.signIn")} · {AGENT_NAMES[loginAgent]}</strong>
              <p className="m-0 text-sm text-muted">{t(LOGIN_GUIDANCE[loginAgent])}</p>
            </div>
            <Button size="xs" variant="default" onClick={() => uiActions.navigate({ kind: "agents", agent: loginAgent, section: "account" })}>
              {t("agents.checkAccount")}
            </Button>
          </div>
        )}
        <SessionTerminal
          sessionId={session.id}
          plain={terminal}
          archived={session.archived}
          onSize={(next) => (size.current = next)}
          focusKey={terminal ? undefined : `${session.status.state}:${session.lastStartedAt ?? ""}`}
          notice={!terminal && !live ? <>
            <span>
              {session.status.state === "failed"
                ? session.status.message
                : loginAgent ? t(session.status.state === "exited" ? "agents.loginExited" : "agents.loginStopped")
                : session.status.state === "exited"
                  ? `The agent exited${session.status.code != null ? ` with code ${session.status.code}` : ""}.`
                  : "This session is not running."}
            </span>
            {loginAgent == null && session.lastStartedAt != null && (
              <Button size="xs" leftSection={<IconPlayerPlay size={14} />} onClick={() => start(true)}>
                Continue conversation
              </Button>
            )}
            <Button size="xs" variant="default" leftSection={<IconRefresh size={14} />} onClick={() => start(false)}>
              {loginAgent ? t("agents.retryLogin") : "Start fresh"}
            </Button>
          </> : null}
        />
      </div>}
      {loginAgent == null && <WorkbenchPanel root={root} git={git} />}
      </div>
    </>
  );
}
