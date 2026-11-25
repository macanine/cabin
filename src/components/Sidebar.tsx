import { ActionIcon, Menu, Tooltip } from "@mantine/core";
import {
  IconActivity, IconArchive, IconArchiveOff, IconBell, IconBrandChrome, IconChartBar, IconCheck,
  IconClock, IconCopy, IconDots, IconDownload, IconFolder, IconFolderPlus, IconLayoutSidebarLeftCollapse,
  IconLayoutSidebarLeftExpand, IconListDetails, IconPencil, IconPlayerPlay, IconPlayerStop, IconPlus, IconSearch,
  IconPlug, IconRobot, IconSettings, IconSparkles, IconTerminal2, IconTrash,
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
import { InternetMark, InternetStatus } from "./network/InternetStatus";
import { NewWorkspaceModal } from "./NewWorkspaceModal";
import { machineLabel } from "./MachineGate";
import { useOperationSummary } from "./OperationCenter";
import { SessionMenuItems } from "./SessionMenu";
import { useClock } from "../lib/clock";
import { useMinute } from "../lib/useNow";
import { IS_MAC } from "../lib/platform";
import { DisclosureChevron, Reveal } from "./motion";

const NEW_TASK_KEY = IS_MAC ? "⌘N" : "Ctrl+Shift+N";
const SEARCH_KEY = IS_MAC ? "⌘K" : "Ctrl+Shift+K";

const MODE_ICON: Record<TaskListMode, ReactNode> = {
  project: <IconFolder size={15} />,
  session: <IconListDetails size={15} />,
  recent: <IconClock size={15} />,
  unread: <IconBell size={15} />,
};

export function InlineRename({
  value,
  onDone,
  className = "min-w-0 flex-1 rounded border border-line bg-surface px-1 py-px font-[inherit] text-fg outline-none focus:border-[color:var(--field-focus-border)]",
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
  const [tone, label] = activity === "waiting" ? ["bg-warn", "Waiting for you"]
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
    <div className="press group flex h-8 cursor-pointer items-center gap-2.5 rounded-lg pr-1 pl-2 text-[13px] font-medium text-muted hover:bg-hover hover:text-fg" role="button" tabIndex={0} onClick={onToggle} onKeyDown={(e) => { if (e.target === e.currentTarget && (e.key === "Enter" || e.key === " ")) { e.preventDefault(); onToggle(); } }} aria-expanded={open}>
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

const LIST_TITLE: Record<TaskListMode, string> = { project: "Projects", session: "Tasks", recent: "Recent", unread: "Inbox" };

function TaskList() {
  const workspaces = useApp((state) => state.workspaces);
  const sessions = useApp((state) => state.sessions);
  const mode = useUi((ui) => ui.taskListMode);
  const unread = useUnread();
  const now = useMinute();
  useClock(); // ages move when the guest clock offset is measured
  const [showArchived, setShowArchived] = useState(false);
  const [closed, setClosed] = useState(readCollapsedProjects);
  const [creating, setCreating] = useState(false);

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

  const sections = taskSections(mode, sessions, workspaces, { showArchived, unread, now });
  const projectName = (id: string) => workspaces.find((w) => w.id === id)?.name ?? "";
  const activity = useActivity();
  const waiting = sessions.filter((session) => !session.archived && activity.get(session.id) === "waiting");
  const empty = sections.every((s) => s.sessions.length === 0) && !(mode === "project" && sections.length > 0);

  return (
    <>
      <div className="flex h-8 items-center gap-0.5 pr-2.5 pl-4 text-xs font-medium text-faint">
        <span className="min-w-0 flex-1">{LIST_TITLE[mode]}</span>
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

      <nav className="min-h-0 flex-1 overflow-y-auto px-2 pt-0.5 pb-2.5" aria-label="Tasks">
        {waiting.length > 0 && (
          <div className="mb-2 rounded-xl bg-warn/10 p-1" role="group" aria-label="Needs you">
            <div className="px-2 pt-1 pb-0.5 text-xs font-medium text-warn">Needs you</div>
            {waiting.map((session) => <TaskItem key={session.id} session={session} unread={false} now={now} showProject={projectName(session.workspaceId)} />)}
          </div>
        )}
        {sections.map((section) => {
          const open = !section.workspace || !closed.has(section.workspace.id);
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
            {mode === "unread" ? "Nothing unread." : "Tasks you start appear here."}
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

function AccountBar({ collapsed, today }: { collapsed: boolean; today: string | null }) {
  const machine = useApp((state) => state.machine);
  const user = machine?.username ?? "dev";
  const host = machine?.hostname ?? (machine?.state === "notInstalled" ? "not installed" : "vm");
  const connected = machine?.connection === "connected";
  const tone = connected ? "ok" : machine?.busy || machine?.state === "running" ? "warn" : machine?.error ? "bad" : "";
  return (
    <div className={`flex shrink-0 items-center ${collapsed ? "flex-col justify-center gap-1 py-2" : "h-12 gap-2 pr-3 pl-4"}`}>
      {collapsed ? (machine?.state === "running" && connected && <InternetMark />) : (
        <span className="flex min-w-0 items-center gap-2.5 truncate text-[13px] font-medium" title={`${user}@${host} · ${machineLabel(machine)}`}>
          <span className="grid size-5 shrink-0 place-items-center"><span className={`dot ${tone}`} aria-label={machineLabel(machine)} /></span>
          <span className="truncate">{user}@{host}</span>
        </span>
      )}
      {!collapsed && <span className="flex-1" />}
      {!collapsed && today && (
        <button type="button" className="press shrink-0 cursor-pointer rounded-md border-0 bg-transparent px-1.5 py-1 text-xs text-faint tabular-nums hover:bg-hover hover:text-fg" title={today} aria-label={today} onClick={() => uiActions.navigate({ kind: "usage" })}>{today.replace(/^Today /, "")}</button>
      )}
      <SettingsMenu />
    </div>
  );
}

// -------------------------------------------------------------------- shell

type ToolKind = "chrome" | "agents" | "usage" | "mcp" | "skills";
const TOOLS: { kind: ToolKind; label: string; icon: typeof IconRobot }[] = [
  { kind: "chrome", label: "Chrome", icon: IconBrandChrome },
  { kind: "agents", label: "Agents", icon: IconRobot },
  { kind: "usage", label: "Usage", icon: IconChartBar },
  { kind: "mcp", label: "MCP", icon: IconPlug },
  { kind: "skills", label: "Skills", icon: IconSparkles },
];
const RAIL_STEP = 40; // collapsed: 38px button + 2px gap

/**
 * The tools every task shares, as labelled tabs under the task list (a
 * column of icons when collapsed). One pill glides to the open one.
 */
function ToolDock({ collapsed }: { collapsed: boolean }) {
  const view = useUi((ui) => ui.view.kind);
  const attention = useApp((state) => attentionCount(state.agents));
  const today = todayLabel(useUsage().todayCost);
  const index = TOOLS.findIndex((tool) => tool.kind === view);
  const at = Math.max(index, 0);
  return (
    <nav aria-label="Tools" className={`relative mx-2 shrink-0 ${collapsed ? "flex flex-col gap-0.5" : "grid grid-cols-5"}`}>
      <span
        aria-hidden
        className="glass pointer-events-none absolute top-0 left-0 rounded-lg transition-[translate,opacity] duration-[var(--motion-glide)] ease-[var(--ease-spring)]"
        style={{
          width: collapsed ? "100%" : "20%", height: collapsed ? RAIL_STEP - 2 : "100%",
          translate: collapsed ? `0 ${at * RAIL_STEP}px` : `${at * 100}% 0`, opacity: index < 0 ? 0 : 1,
        }}
      />
      {TOOLS.map(({ kind, label, icon: Icon }) => {
        const active = view === kind;
        const alert = kind === "agents" && attention > 0;
        return (
          <button
            key={kind}
            type="button"
            title={kind === "usage" && today ? today : label}
            aria-label={alert ? `${label}, ${attention} need attention` : label}
            aria-current={active ? "page" : undefined}
            onClick={() => uiActions.navigate({ kind })}
            className={`press group relative z-[1] flex cursor-pointer flex-col items-center justify-center gap-0.5 rounded-lg border-0 bg-transparent p-0 ${collapsed ? "h-[38px]" : "h-12"} ${active ? "text-fg" : "text-muted hover:text-fg"}`}
          >
            <span className={`relative grid size-5 place-items-center ${active ? "text-accent" : ""}`}>
              <Icon size={18} stroke={1.7} />
              {alert && <span className="absolute -top-0.5 -right-0.5 size-[7px] rounded-full bg-warn" aria-hidden />}
            </span>
            {!collapsed && <span className="max-w-full truncate text-[11px] font-medium">{label}</span>}
          </button>
        );
      })}
    </nav>
  );
}

/** Start something, or find something: the two things the sidebar opens with. */
function StartRow({ collapsed }: { collapsed: boolean }) {
  const active = useUi((ui) => ui.view.kind === "new");
  const icon = "press grid size-9 shrink-0 cursor-pointer place-items-center rounded-[10px] border-0 bg-transparent p-0 text-muted hover:bg-hover hover:text-fg";
  const fresh = () => uiActions.navigate({ kind: "new" });
  const search = (
    <Tooltip label={`Search (${SEARCH_KEY})`} position={collapsed ? "right" : "bottom"}>
      <button type="button" className={icon} aria-label="Search tasks and pages" onClick={uiActions.openPalette}><IconSearch size={17} stroke={1.8} /></button>
    </Tooltip>
  );
  if (collapsed) {
    return (
      <div className="flex shrink-0 flex-col items-center gap-1">
        <Tooltip label={`New task (${NEW_TASK_KEY})`} position="right">
          <button type="button" className="press grid size-9 cursor-pointer place-items-center rounded-[10px] border-0 bg-accent-fill p-0 text-white hover:bg-accent-fill/88" aria-label="New task" aria-current={active ? "page" : undefined} onClick={fresh}><IconPlus size={17} stroke={2.2} /></button>
        </Tooltip>
        {search}
      </div>
    );
  }
  return (
    <div className="mx-2 mb-2 flex shrink-0 items-center gap-1">
      <button type="button" aria-current={active ? "page" : undefined} onClick={fresh}
        className="press flex h-9 min-w-0 flex-1 cursor-pointer items-center gap-2.5 rounded-[10px] border-0 bg-accent-fill px-2 text-left text-md font-medium text-white hover:bg-accent-fill/88">
        <span className="grid size-5 shrink-0 place-items-center"><IconPlus size={16} stroke={2.2} /></span>
        <span className="min-w-0 flex-1 truncate">New task</span>
        <span className="pr-1 text-xs text-white/75">{NEW_TASK_KEY}</span>
      </button>
      {search}
    </div>
  );
}

/** Tasks first; the shared tools and the machine's state sit below them. */
export function Sidebar() {
  const collapsed = useUi((ui) => ui.sidebarCollapsed);
  const connected = useApp((state) => state.machine?.state === "running" && state.machine.connection === "connected");
  const today = todayLabel(useUsage().todayCost);
  return (
    <aside className={`sidebar${collapsed ? " collapsed" : ""} row-start-2 flex min-h-0 flex-col pt-0.5 transition-[width] duration-300 ease-out select-none`} style={{ width: collapsed ? "var(--sidebar-collapsed-width)" : "var(--sidebar-width)" }}>
      <StartRow collapsed={collapsed} />
      {collapsed ? <div className="flex-1" /> : <TaskList />}
      <ToolDock collapsed={collapsed} />
      {connected && !collapsed && (
        <div className="glass mx-2 mt-2 grid min-w-0 shrink-0 rounded-[var(--card-radius)]">
          <InternetStatus />
          <PerformancePanel />
        </div>
      )}
      <AccountBar collapsed={collapsed} today={today} />
    </aside>
  );
}
