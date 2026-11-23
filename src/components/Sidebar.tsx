import { ActionIcon, Menu, Tooltip } from "@mantine/core";
import {
  IconActivity, IconArchive, IconArchiveOff, IconBell, IconBrandChrome, IconChartBar, IconCheck,
  IconClock, IconCopy, IconDots, IconDownload, IconFolder, IconFolderPlus, IconLayoutSidebarLeftCollapse,
  IconLayoutSidebarLeftExpand, IconListDetails, IconPencil, IconPlayerPlay, IconPlayerStop, IconPlus, IconSearch,
  IconPlug, IconRobot, IconSettings, IconSparkles, IconTerminal2, IconTrash, IconX,
  IconRefresh,
} from "@tabler/icons-react";
import { type ReactNode, useEffect, useRef, useState } from "react";
import type { Session, Workspace } from "../services/ipc/generated/core";
import { libraryIpc } from "../services/ipc/library";
import { machineIpc } from "../services/ipc/app";
import { confirmAction } from "../services/ipc/dialogs";
import { refreshSyncs, useApp } from "../state/store";
import { syncIpc } from "../services/ipc/sync";
import { SyncDot, SyncPanel } from "./sync/SyncPanel";
import { uiActions, useUi } from "../state/ui";
import { useActivity, useUnread } from "../state/unread";
import { useMacPermissions } from "../state/macPermissions";
import { attempt } from "../lib/operations";
import { attentionCount } from "../lib/agentsHub";
import { todayLabel } from "../views/UsageView";
import { useUsage } from "./usage/UsageMonitor";
import { openTerminal } from "../lib/terminal";
import { shortAge, TASK_LIST_MODES, taskSections, type TaskListMode, lastActivity } from "../lib/taskList";
import { sessionKindName } from "./common";
import { PerformancePanel } from "./MonitorPanel";
import { InternetStatus } from "./network/InternetStatus";
import { NewWorkspaceModal } from "./NewWorkspaceModal";
import { machineLabel } from "./MachineGate";
import { useOperationSummary } from "./OperationCenter";
import { SessionMenuItems } from "./SessionMenu";
import { useClock } from "../lib/clock";
import { useMinute } from "../lib/useNow";
import { DisclosureChevron, Reveal } from "./motion";

export const SEARCH_INPUT_ID = "sidebar-search";
/** Dispatched on window to open and focus the task search (Ctrl+Shift+K). */
export const SEARCH_EVENT = "cabin:search";
export const IS_MAC = typeof navigator !== "undefined" && /Mac/.test(navigator.userAgent);
const NEW_TASK_KEY = IS_MAC ? "⌘N" : "Ctrl+Shift+N";

const MODE_ICON: Record<TaskListMode, ReactNode> = {
  project: <IconFolder size={15} />,
  session: <IconListDetails size={15} />,
  recent: <IconClock size={15} />,
  unread: <IconBell size={15} />,
};

export function InlineRename({
  value,
  onDone,
  className = "min-w-0 flex-1 rounded border border-accent bg-surface px-1 py-px font-[inherit] text-fg outline-none",
}: {
  value: string;
  onDone: (next: string | null) => void;
  className?: string;
}) {
  const [text, setText] = useState(value);
  const ref = useRef<HTMLInputElement>(null);
  const done = useRef(false);
  useEffect(() => {
    ref.current?.focus();
    ref.current?.select();
  }, []);
  const finish = (save: boolean) => {
    if (done.current) return;
    done.current = true;
    const next = text.trim();
    onDone(save && next && next !== value ? next : null);
  };
  return (
    <input
      ref={ref}
      className={className}
      value={text}
      aria-label="New name"
      onChange={(e) => setText(e.currentTarget.value)}
      onClick={(e) => e.stopPropagation()}
      onKeyDown={(e) => {
        e.stopPropagation();
        if (e.key === "Enter") finish(true);
        if (e.key === "Escape") finish(false);
      }}
      onBlur={() => finish(true)}
    />
  );
}

// ----------------------------------------------------------- window chrome

/** Collapses or expands the sidebar; lives in the window toolbar. */
export function SidebarToggle() {
  const collapsed = useUi((ui) => ui.sidebarCollapsed);
  const label = collapsed ? "Expand sidebar" : "Collapse sidebar";
  return (
    <Tooltip label={IS_MAC ? label : `${label} (Ctrl+Shift+B)`} position="bottom">
      <ActionIcon variant="subtle" color="gray" size="md" onClick={uiActions.toggleSidebar} aria-label={label}>
        {collapsed ? <IconLayoutSidebarLeftExpand size={18} stroke={1.6} /> : <IconLayoutSidebarLeftCollapse size={18} stroke={1.6} />}
      </ActionIcon>
    </Tooltip>
  );
}

// --------------------------------------------------------------- task items

/** The task's state, in the icon column: running pulses, waiting and unread are accent dots. */
function TaskMarker({ session, unread }: { session: Session; unread: boolean }) {
  const activity = useActivity().get(session.id);
  const live = session.agent !== null && (session.status.state === "running" || session.status.state === "starting");
  const [tone, label] = activity === "waiting" ? ["bg-accent", "Waiting for you"]
    : live ? ["bg-ok live-dot", "Running"]
      : unread ? ["bg-accent", "Unread"]
        : ["", ""];
  return (
    <span className="grid size-5 shrink-0 place-items-center" aria-hidden={!label}>
      {label && <span className={`size-[7px] rounded-full ${tone} animate-settle`} aria-label={label} title={label} />}
    </span>
  );
}

function TaskItem({ session, unread, now, showProject }: { session: Session; unread: boolean; now: number; showProject?: string }) {
  const active = useUi((ui) => ui.view.kind === "session" && ui.view.id === session.id);
  const [renaming, setRenaming] = useState(false);
  const kind = sessionKindName(session.agent);
  return (
    <div
      className={`press group flex h-8 w-full cursor-pointer items-center gap-2.5 rounded-lg pr-1 pl-2 text-[13px] text-fg ${active ? "bg-sel" : "hover:bg-hover"}`}
      role="button"
      tabIndex={0}
      title={showProject ? `${session.name} — ${kind} · ${showProject}` : `${session.name} — ${kind}`}
      onClick={() => uiActions.navigate({ kind: "session", id: session.id })}
      onKeyDown={(e) => e.key === "Enter" && uiActions.navigate({ kind: "session", id: session.id })}
    >
      <TaskMarker session={session} unread={unread} />
      {renaming ? (
        <InlineRename
          value={session.name}
          onDone={(next) => {
            setRenaming(false);
            if (next) void attempt(libraryIpc.updateSession(session.id, { name: next }));
          }}
        />
      ) : (
        <span className={`min-w-0 flex-1 truncate ${unread ? "font-semibold" : ""}`}>
          {session.name}
          {session.archived && <span className="text-faint"> · archived</span>}
        </span>
      )}
      <span className="pr-1 text-xs whitespace-nowrap text-faint tabular-nums group-hover:hidden group-focus-within:hidden">{shortAge(lastActivity(session), now)}</span>
      <Menu position="bottom-end" withinPortal shadow="md">
        <Menu.Target>
          <ActionIcon
            className="hidden group-hover:inline-flex group-focus-within:inline-flex data-expanded:inline-flex"
            size="sm"
            variant="subtle"
            color="gray"
            aria-label={`Actions for ${session.name}`}
            onClick={(e) => e.stopPropagation()}
          >
            <IconDots size={14} />
          </ActionIcon>
        </Menu.Target>
        <Menu.Dropdown onClick={(e) => e.stopPropagation()}>
          <SessionMenuItems session={session} onRename={() => setRenaming(true)} />
        </Menu.Dropdown>
      </Menu>
    </div>
  );
}

function ProjectHeader({
  workspace,
  count,
  open,
  onToggle,
}: {
  workspace: Workspace;
  count: number;
  open: boolean;
  onToggle: () => void;
}) {
  const [renaming, setRenaming] = useState(false);
  const [syncOpen, setSyncOpen] = useState(false);
  const sync = useApp((state) => state.syncs.find((view) => view.project.workspaceId === workspace.id) ?? null);
  return (
    <>
    {/* Outside the row: React events bubble through portals, and a click in
        the dialog must not fold the project. */}
    {sync && <SyncPanel view={sync} opened={syncOpen} onClose={() => setSyncOpen(false)} />}
    <div className="press group flex h-8 cursor-pointer items-center gap-2.5 rounded-lg pr-1 pl-2 text-[13px] font-semibold text-fg hover:bg-hover" role="button" tabIndex={0} onClick={onToggle} onKeyDown={(e) => { if (e.target === e.currentTarget && (e.key === "Enter" || e.key === " ")) { e.preventDefault(); onToggle(); } }} aria-expanded={open}>
      <span className="grid size-5 shrink-0 place-items-center"><DisclosureChevron open={open} className="text-faint" /></span>
      {renaming ? (
        <InlineRename
          value={workspace.name}
          onDone={(next) => {
            setRenaming(false);
            if (next) void attempt(libraryIpc.updateWorkspace(workspace.id, { name: next }));
          }}
        />
      ) : (
        <span className="min-w-0 flex-1 truncate">
          {workspace.name}
          {workspace.archived && <span className="text-faint"> · archived</span>}
        </span>
      )}
      {sync && <SyncDot view={sync} onClick={() => setSyncOpen(true)} />}
      <span className="pr-1 text-xs text-faint tabular-nums group-hover:hidden">{count || ""}</span>
      <Tooltip label="New task in this project">
        <ActionIcon
          className="hidden group-hover:inline-flex group-focus-within:inline-flex data-expanded:inline-flex"
          size="sm"
          variant="subtle"
          color="gray"
          aria-label={`New task in ${workspace.name}`}
          onClick={(e) => {
            e.stopPropagation();
            uiActions.navigate({ kind: "new", workspaceId: workspace.id });
          }}
        >
          <IconPlus size={14} />
        </ActionIcon>
      </Tooltip>
      <Menu position="bottom-end" withinPortal shadow="md">
        <Menu.Target>
          <ActionIcon
            className="hidden group-hover:inline-flex group-focus-within:inline-flex data-expanded:inline-flex"
            size="sm"
            variant="subtle"
            color="gray"
            aria-label={`Actions for ${workspace.name}`}
            onClick={(e) => e.stopPropagation()}
          >
            <IconDots size={14} />
          </ActionIcon>
        </Menu.Target>
        <Menu.Dropdown onClick={(e) => e.stopPropagation()}>
          <Menu.Label className="truncate">{workspace.path}</Menu.Label>
          <Menu.Item leftSection={<IconPlus size={14} />} onClick={() => uiActions.navigate({ kind: "new", workspaceId: workspace.id })}>
            New task here
          </Menu.Item>
          <Menu.Item leftSection={<IconTerminal2 size={14} />} onClick={() => openTerminal(workspace.id)}>
            New terminal
          </Menu.Item>
          {sync && (
            <Menu.Item leftSection={<IconRefresh size={14} />} onClick={() => setSyncOpen(true)}>
              Sync local folder…
            </Menu.Item>
          )}
          <Menu.Item leftSection={<IconPencil size={14} />} onClick={() => setRenaming(true)}>
            Rename
          </Menu.Item>
          <Menu.Item leftSection={<IconCopy size={14} />} onClick={() => attempt(libraryIpc.duplicateWorkspace(workspace.id), "Project duplicated")}>
            Duplicate
          </Menu.Item>
          <Menu.Item
            leftSection={workspace.archived ? <IconArchiveOff size={14} /> : <IconArchive size={14} />}
            onClick={() => attempt(libraryIpc.updateWorkspace(workspace.id, { archived: !workspace.archived }))}
          >
            {workspace.archived ? "Unarchive" : "Archive"}
          </Menu.Item>
          <Menu.Divider />
          <Menu.Item
            color="red"
            leftSection={<IconTrash size={14} />}
            onClick={async () => {
              if (!await confirmAction(`Delete the project “${workspace.name}” and its tasks? Files in ${workspace.path}${sync ? ` and ${sync.project.hostPath}` : ""} are not touched.`)) return;
              if (sync) await attempt(syncIpc.remove(sync.project.id));
              await attempt(libraryIpc.deleteWorkspace(workspace.id), "Project deleted");
              void refreshSyncs();
            }}
          >
            Delete project
          </Menu.Item>
        </Menu.Dropdown>
      </Menu>
    </div>
    </>
  );
}

// ---------------------------------------------------------------- task list

const COLLAPSED_PROJECTS_KEY = "cabin.collapsedProjects";

function readCollapsedProjects(): Set<string> {
  try {
    return new Set(JSON.parse(localStorage.getItem(COLLAPSED_PROJECTS_KEY) ?? "[]") as string[]);
  } catch {
    return new Set();
  }
}

function TaskList() {
  const workspaces = useApp((state) => state.workspaces);
  const sessions = useApp((state) => state.sessions);
  const mode = useUi((ui) => ui.taskListMode);
  const unread = useUnread();
  const now = useMinute();
  useClock(); // ages move when the guest clock offset is measured
  const [searching, setSearching] = useState(false);
  const [query, setQuery] = useState("");
  const [showArchived, setShowArchived] = useState(false);
  const [closed, setClosed] = useState(readCollapsedProjects);
  const [creating, setCreating] = useState(false);

  useEffect(() => {
    const open = () => {
      setSearching(true);
      requestAnimationFrame(() => document.getElementById(SEARCH_INPUT_ID)?.focus());
    };
    window.addEventListener(SEARCH_EVENT, open);
    return () => window.removeEventListener(SEARCH_EVENT, open);
  }, []);

  const toggleProject = (id: string) => {
    const next = new Set(closed);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    setClosed(next);
    try {
      localStorage.setItem(COLLAPSED_PROJECTS_KEY, JSON.stringify([...next]));
    } catch {
      // Not remembered in this viewer.
    }
  };

  const sections = taskSections(mode, sessions, workspaces, { query, showArchived, unread, now });
  const projectName = (id: string) => workspaces.find((w) => w.id === id)?.name ?? "";
  const empty = sections.every((s) => s.sessions.length === 0) && !(mode === "project" && sections.length > 0);

  return (
    <>
      <div className="flex h-8 items-center gap-0.5 pt-2 pr-2.5 pl-4 text-xs font-semibold text-faint">
        <span className="min-w-0 flex-1">Tasks</span>
        <Tooltip label="Search tasks">
          <ActionIcon
            size="sm"
            variant={searching ? "light" : "subtle"}
            color="gray"
            aria-label="Search tasks"
            onClick={() => {
              setSearching(!searching);
              if (searching) setQuery("");
              else requestAnimationFrame(() => document.getElementById(SEARCH_INPUT_ID)?.focus());
            }}
          >
            <IconSearch size={14} />
          </ActionIcon>
        </Tooltip>
        <Menu position="bottom-end" withinPortal shadow="md" width={210}>
          <Menu.Target>
            <ActionIcon size="sm" variant="subtle" color="gray" aria-label="Task list options">
              <IconDots size={14} />
            </ActionIcon>
          </Menu.Target>
          <Menu.Dropdown>
            <Menu.Item leftSection={<IconFolderPlus size={15} />} onClick={() => setCreating(true)}>New project…</Menu.Item>
            <Menu.Divider />
            <Menu.Label>Group by</Menu.Label>
            {TASK_LIST_MODES.map((item) => (
              <Menu.Item
                key={item.mode}
                leftSection={MODE_ICON[item.mode]}
                rightSection={mode === item.mode ? <IconCheck size={14} /> : undefined}
                onClick={() => uiActions.setTaskListMode(item.mode)}
              >
                {item.label}
              </Menu.Item>
            ))}
            <Menu.Divider />
            <Menu.Item
              leftSection={<IconArchive size={15} />}
              rightSection={showArchived ? <IconCheck size={14} /> : undefined}
              onClick={() => setShowArchived(!showArchived)}
            >
              Show archived
            </Menu.Item>
          </Menu.Dropdown>
        </Menu>
      </div>

      {searching && (
        <label className="mx-2.5 mt-1 mb-1.5 flex items-center gap-1.5 rounded-lg border border-line bg-surface/70 px-2 text-faint transition-[border-color,box-shadow] duration-200 focus-within:border-accent focus-within:shadow-[0_0_0_3px_var(--accent-soft)]">
          <IconSearch size={14} />
          <input
            id={SEARCH_INPUT_ID}
            className="w-full border-0 bg-transparent py-1.5 font-[inherit] text-[13px] text-fg outline-none"
            placeholder="Search tasks and projects"
            value={query}
            onChange={(e) => setQuery(e.currentTarget.value)}
            onKeyDown={(e) => {
              if (e.key === "Escape") {
                setQuery("");
                setSearching(false);
              }
            }}
            aria-label="Search tasks"
          />
          {query && (
            <button className="grid cursor-pointer place-items-center border-0 bg-transparent p-0.5 text-faint hover:text-fg" aria-label="Clear search" onClick={() => setQuery("")}>
              <IconX size={12} />
            </button>
          )}
        </label>
      )}

      <nav className="min-h-0 flex-1 overflow-y-auto px-2 pt-0.5 pb-2.5" aria-label="Tasks">
        {sections.map((section) => {
          const open = !section.workspace || !closed.has(section.workspace.id) || query !== "";
          return (
            <div key={section.key} className="[&+&]:mt-1">
              {section.workspace ? (
                <ProjectHeader
                  workspace={section.workspace}
                  count={section.sessions.length}
                  open={open}
                  onToggle={() => toggleProject(section.workspace!.id)}
                />
              ) : (
                section.label && <div className="px-2 pt-2.5 pb-0.5 text-xs font-semibold text-faint">{section.label}</div>
              )}
              <Reveal open={open}>
                {section.sessions.map((session) => (
                  <TaskItem
                    key={session.id}
                    session={session}
                    unread={unread.has(session.id)}
                    now={now}
                    showProject={mode === "project" ? undefined : projectName(session.workspaceId)}
                  />
                ))}
                {section.workspace && section.sessions.length === 0 && <div className="py-1 pl-[38px] text-xs text-faint">No tasks yet</div>}
              </Reveal>
            </div>
          );
        })}
        {empty && (
          <p className="m-0 px-2.5 py-1 text-[13px] text-faint">
            {query ? "No tasks match." : mode === "unread" ? "Nothing unread." : "Tasks you start appear here."}
          </p>
        )}
      </nav>
      <NewWorkspaceModal opened={creating} onClose={() => setCreating(false)} />
    </>
  );
}

// ------------------------------------------------------------------- bottom

function SettingsMenu() {
  const machine = useApp((state) => state.machine);
  const { running } = useOperationSummary();
  const { projects } = useMacPermissions();
  const needsAccess = projects?.some(({ access }) => access.state !== "available");
  return (
    <Menu position="top-end" width={220} withinPortal shadow="md">
      <Menu.Target>
        <ActionIcon variant="subtle" color={needsAccess ? "yellow" : "gray"} aria-label="Settings and machine" title={needsAccess ? "A local project folder needs attention" : undefined}>
          <IconSettings size={17} />
        </ActionIcon>
      </Menu.Target>
      <Menu.Dropdown>
        <Menu.Label>{machineLabel(machine)}</Menu.Label>
        {machine && machine.state === "running" && (
          <Menu.Item leftSection={<IconPlayerStop size={15} />} disabled={machine.busy != null} onClick={() => attempt(machineIpc.stop(), "Machine stopped")}>
            Stop machine
          </Menu.Item>
        )}
        {machine && machine.state === "stopped" && (
          <Menu.Item leftSection={<IconPlayerPlay size={15} />} disabled={machine.busy != null} onClick={() => attempt(machineIpc.start(), "Machine started")}>
            Start machine
          </Menu.Item>
        )}
        {machine && machine.state === "notInstalled" && (
          <Menu.Item leftSection={<IconDownload size={15} />} onClick={() => uiActions.navigate({ kind: "install" })}>
            {machine.busy ? "Install progress" : "Install machine…"}
          </Menu.Item>
        )}
        <Menu.Item leftSection={<IconActivity size={15} />} onClick={() => uiActions.showOperations(true)}>
          Operations
          {running > 0 && <span className="float-right ml-3 text-xs text-faint">{running} running</span>}
        </Menu.Item>
        <Menu.Divider />
        <Menu.Item leftSection={<IconFolder size={15} />} rightSection={needsAccess && <span className="dot warn" aria-label="Needs attention" />}
          onClick={() => uiActions.navigate({ kind: "settings", tab: "permissions" })}>Folder permissions</Menu.Item>
        <Menu.Item leftSection={<IconSettings size={15} />} onClick={() => uiActions.navigate({ kind: "settings" })}>
          Settings
          {IS_MAC && <span className="float-right ml-3 text-xs text-faint">⌘,</span>}
        </Menu.Item>
      </Menu.Dropdown>
    </Menu>
  );
}

function AccountBar({ collapsed }: { collapsed: boolean }) {
  const machine = useApp((state) => state.machine);
  const user = machine?.username ?? "dev";
  const host = machine?.hostname ?? (machine?.state === "notInstalled" ? "not installed" : "vm");
  const connected = machine?.connection === "connected";
  const tone = connected ? "ok" : machine?.busy || machine?.state === "running" ? "warn" : machine?.error ? "bad" : "";
  return (
    <div className={`flex h-12 shrink-0 items-center gap-2 border-t border-line/70 px-4 ${collapsed ? "flex-col justify-center" : ""}`}>
      {collapsed ? <span className={`dot ${tone}`} aria-label={machineLabel(machine)} title={`${user}@${host} · ${machineLabel(machine)}`} /> : (
        <span className="flex min-w-0 items-center gap-2.5 truncate text-[13px] font-medium" title={`${user}@${host} · ${machineLabel(machine)}`}>
          <span className="grid size-5 shrink-0 place-items-center"><span className={`dot ${tone}`} aria-label={machineLabel(machine)} /></span>
          <span className="truncate">{user}@{host}</span>
        </span>
      )}
      {!collapsed && <span className="flex-1" />}
      <SettingsMenu />
    </div>
  );
}

// -------------------------------------------------------------------- shell

const NAV_ROW = 34; // 32px row + 2px gap: the indicator's step

/** One row of the sidebar's navigation: icon, label, optional meta; icon only when collapsed. */
function NavButton({ icon, label, active, collapsed, title, onClick, accent, children }: {
  icon: ReactNode;
  label: string;
  active: boolean;
  collapsed: boolean;
  title?: string;
  onClick: () => void;
  accent?: boolean;
  children?: ReactNode;
}) {
  return (
    <button
      type="button"
      title={title ?? label}
      aria-label={label}
      aria-current={active ? "page" : undefined}
      onClick={onClick}
      className={`press group relative z-[1] mx-2 mb-0.5 flex h-8 cursor-pointer items-center gap-2.5 rounded-lg border-0 bg-transparent px-2 text-left text-md font-medium text-fg w-[calc(100%-16px)] ${
        collapsed ? "justify-center" : ""
      } ${active ? "" : "hover:bg-hover"}`}
    >
      <span className={`grid size-5 shrink-0 place-items-center transition-colors duration-200 ${accent ? "rounded-full bg-accent text-white" : active ? "text-accent" : "text-muted group-hover:text-fg"}`}>{icon}</span>
      {!collapsed && <span className="min-w-0 flex-1 truncate">{label}</span>}
      {!collapsed && children}
    </button>
  );
}

/** The selection: one glass pill that glides between rows instead of jumping. */
function NavIndicator({ index }: { index: number }) {
  return (
    <span
      aria-hidden
      className="glass pointer-events-none absolute inset-x-2 top-0 h-8 rounded-lg transition-[translate,opacity] duration-[var(--motion-glide)] ease-[var(--ease-spring)]"
      style={{ translate: `0 ${Math.max(index, 0) * NAV_ROW}px`, opacity: index < 0 ? 0 : 1 }}
    />
  );
}

type NavKind = "new" | "chrome" | "agents" | "usage" | "mcp" | "skills";
const NAV_ORDER: NavKind[] = ["new", "chrome", "agents", "usage", "mcp", "skills"];

export function Sidebar() {
  const collapsed = useUi((ui) => ui.sidebarCollapsed);
  const view = useUi((ui) => ui.view.kind);
  const agentAttention = useApp((state) => attentionCount(state.agents));
  const today = todayLabel(useUsage().todayCost);
  const go = (kind: NavKind) => () => uiActions.navigate({ kind });
  const nav = { collapsed };
  return (
    <aside className={`sidebar${collapsed ? " collapsed" : ""} row-start-2 flex min-h-0 flex-col gap-px pt-0.5 transition-[width] duration-300 ease-out select-none`} style={{ width: collapsed ? "var(--sidebar-collapsed-width)" : "var(--sidebar-width)" }}>
      <div className="relative shrink-0">
        <NavIndicator index={NAV_ORDER.indexOf(view as NavKind)} />
        <NavButton {...nav} accent icon={<IconPlus size={13} stroke={2.6} />} label="New Task" title={`New task (${NEW_TASK_KEY})`} active={view === "new"} onClick={go("new")} />
        <NavButton {...nav} icon={<IconBrandChrome size={18} stroke={1.7} />} label="Chrome" active={view === "chrome"} onClick={go("chrome")} />
        <NavButton {...nav} icon={<IconRobot size={18} stroke={1.7} />} label="Agents" title="Agents, accounts and usage" active={view === "agents"} onClick={go("agents")}>
          {agentAttention > 0 && <span className="dot warn" aria-label={`${agentAttention} need attention`} />}
        </NavButton>
        <NavButton {...nav} icon={<IconChartBar size={18} stroke={1.7} />} label="Usage" title={today ?? "Usage"} active={view === "usage"} onClick={go("usage")}>
          {today && <span className="text-xs text-faint tabular-nums">{today}</span>}
        </NavButton>
        <NavButton {...nav} icon={<IconPlug size={18} stroke={1.7} />} label="MCP" active={view === "mcp"} onClick={go("mcp")} />
        <NavButton {...nav} icon={<IconSparkles size={18} stroke={1.7} />} label="Skills" active={view === "skills"} onClick={go("skills")} />
      </div>
      {collapsed ? <div className="flex-1" /> : <TaskList />}
      {collapsed ? <InternetStatus collapsed /> : (
        <div className="glass mx-2 my-1 grid min-w-0 shrink-0 rounded-[var(--card-radius)]">
          <InternetStatus collapsed={false} />
          <PerformancePanel />
        </div>
      )}
      <AccountBar collapsed={collapsed} />
    </aside>
  );
}
