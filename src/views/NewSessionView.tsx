import { Button, Menu } from "@mantine/core";
import { IconPlayerPlay, IconPlayerStop, IconSettings, IconRobot, IconServer } from "@tabler/icons-react";
import { type ComponentProps, useState } from "react";
import { machineIpc } from "../services/ipc/app";
import { useApp } from "../state/store";
import { uiActions } from "../state/ui";
import { attempt } from "../lib/operations";
import { attentionCount } from "../lib/agentsHub";
import { selectNetworkSummary } from "../lib/networkSummary";
import { lastActivity, shortAge } from "../lib/taskList";
import { useMinute } from "../lib/useNow";
import { AGENT_KINDS } from "../components/AgentPicker";
import { Composer } from "../components/Composer";
import { sessionKindName } from "../components/common";
import { NewWorkspaceModal } from "../components/NewWorkspaceModal";

type Tone = "ok" | "warn" | "bad" | "off";
const DOT: Record<Tone, string> = { ok: "bg-ok", warn: "bg-warn", bad: "bg-bad", off: "bg-faint" };

/** One live fact about the setup: a status dot, a label, an optional quiet detail. */
const Fact = ({ tone, label, detail, ...rest }: { tone: Tone; label: string; detail?: string | null } & ComponentProps<"button">) => (
    <button type="button" {...rest} className="press inline-flex h-7 min-w-0 shrink cursor-pointer items-center gap-2 rounded-full border border-line/80 bg-transparent px-3 text-left text-xs text-fg hover:bg-hover data-expanded:bg-hover">
      <span className={`size-1.5 shrink-0 rounded-full ${DOT[tone]}`} />
      <span className="shrink-0 font-medium">{label}</span>
      {detail && <span className="min-w-0 truncate text-faint">{detail}</span>}
    </button>
);

/** Section label, inset to line up with the text of the rows below it. */
const SECTION = "mb-1.5 px-3 text-xs font-semibold text-faint";
const ROW = "press flex h-12 w-full cursor-pointer items-center gap-3 rounded-xl border-0 bg-transparent px-3 text-left text-fg hover:bg-hover";

/**
 * The start page, ordered by what matters: the machine's state and whether
 * work can start (top), the box to start it in (centre), then where to carry on
 * (recent tasks, projects).
 */
export function NewSessionView({ workspaceId }: { workspaceId?: string }) {
  const machine = useApp((state) => state.machine);
  const network = useApp(selectNetworkSummary);
  const agents = useApp((state) => state.agents);
  const attention = useApp((state) => attentionCount(state.agents));
  const workspaces = useApp((state) => state.workspaces).filter((w) => !w.archived);
  const sessions = useApp((state) => state.sessions).filter((s) => !s.archived);
  const now = useMinute();
  const [creating, setCreating] = useState(false);
  const recent = [...sessions].sort((a, b) => lastActivity(b) - lastActivity(a)).slice(0, 6);
  const noAgents = agents.length > 0 && agents.every((agent) => !agent.installed);
  const ready = agents.filter((agent) => agent.installed).length;
  const running = machine?.state === "running";
  const stopped = machine?.state === "stopped";
  const wsName = (id: string) => workspaces.find((w) => w.id === id)?.name ?? "";
  const counts = new Map<string, number>();
  for (const s of sessions) counts.set(s.workspaceId, (counts.get(s.workspaceId) ?? 0) + 1);
  const machineLine = machine
    ? [machine.cpus ? `${machine.cpus} CPU` : null, machine.memoryMb ? `${Math.round(machine.memoryMb / 1024)} GB` : null].filter(Boolean).join(" · ")
    : null;

  return (
    <div className="min-h-0 flex-1 overflow-y-auto px-9 pb-12">
      <div className="mx-auto w-full max-w-[880px]">
        <div className="flex min-w-0 items-center justify-center gap-2 pt-5">
          <Menu position="bottom-start" withinPortal shadow="md" width={220}>
            <Menu.Target>
              <Fact
                tone={machine?.busy ? "warn" : running ? (machine?.connection === "connected" ? "ok" : "warn") : stopped ? "off" : "bad"}
                label={machine?.busy ? machine.busy : running ? (machine?.connection === "connected" ? "Machine running" : "Connecting…") : stopped ? "Machine stopped" : "Machine not installed"}
                detail={running ? machineLine : null}
              />
            </Menu.Target>
            <Menu.Dropdown>
              {running && <Menu.Item leftSection={<IconPlayerStop size={14} />} disabled={machine?.busy != null} onClick={() => attempt(machineIpc.stop(), "Machine stopped")}>Stop machine</Menu.Item>}
              {stopped && <Menu.Item leftSection={<IconPlayerPlay size={14} />} disabled={machine?.busy != null} onClick={() => attempt(machineIpc.start(), "Machine started")}>Resume machine</Menu.Item>}
              <Menu.Item leftSection={<IconSettings size={14} />} onClick={() => uiActions.navigate({ kind: "settings", tab: "machine" })}>Machine settings</Menu.Item>
            </Menu.Dropdown>
          </Menu>
          <Fact tone={network.tone === "ok" ? "ok" : network.tone === "bad" ? "bad" : "warn"} label={network.title} detail={network.tone === "ok" ? null : network.detail} onClick={uiActions.openNetworkCheck} />
          <Fact tone={attention > 0 ? "warn" : ready > 0 ? "ok" : "bad"} label={`${ready} of ${AGENT_KINDS.length} agents`} detail={attention > 0 ? `${attention} need attention` : null} onClick={() => uiActions.navigate({ kind: "agents" })} />
        </div>

        <h1 className="mt-[11vh] mb-6 text-center text-[26px] font-semibold tracking-tight">What should an agent work on?</h1>

        {stopped || machine?.state === "notInstalled" ? (
          <div className="glass flex items-center gap-3 rounded-2xl p-4 text-md">
            <IconServer size={20} className="shrink-0 text-warn" />
            <span className="min-w-0 flex-1">{stopped ? "The machine is stopped. Resume it to run agents." : "Install the machine first: agents run inside it."}</span>
            {stopped
              ? <Button size="xs" leftSection={<IconPlayerPlay size={14} />} onClick={() => attempt(machineIpc.start())}>Resume machine</Button>
              : <Button size="xs" onClick={() => uiActions.navigate({ kind: "install" })}>Install</Button>}
          </div>
        ) : noAgents ? (
          <div className="glass flex items-center gap-3 rounded-2xl p-4 text-md">
            <IconRobot size={20} className="shrink-0 text-warn" />
            <span className="min-w-0 flex-1">Install Claude Code, Codex, OpenCode, Pi or Antigravity to get started.</span>
            <Button size="xs" onClick={() => uiActions.navigate({ kind: "agents" })}>Install agents</Button>
          </div>
        ) : (
          <Composer key={workspaceId ?? "any"} workspaceId={workspaceId} />
        )}
        <div className="mt-12 grid grid-cols-[minmax(0,1.5fr)_minmax(0,1fr)] gap-6 max-md:grid-cols-1">
          <section aria-label="Recent tasks">
            <div className={SECTION}>Continue</div>
            <div className="stagger grid">
              {recent.map((session) => {
                const live = session.agent !== null && (session.status.state === "running" || session.status.state === "starting");
                return (
                  <button key={session.id} type="button" className={ROW} onClick={() => uiActions.navigate({ kind: "session", id: session.id })}>
                    <span className="grid min-w-0 flex-1">
                      <span className="truncate text-[13px] font-medium">{session.name}</span>
                      <span className="truncate text-[11.5px] text-faint">{wsName(session.workspaceId)} · {sessionKindName(session.agent)}</span>
                    </span>
                    {live && <span className="live-dot size-1.5 shrink-0 rounded-full bg-ok" aria-label="Running" />}
                    <span className="w-12 shrink-0 text-right text-[11.5px] text-faint tabular-nums">{shortAge(lastActivity(session), now)}</span>
                  </button>
                );
              })}
              {recent.length === 0 && <p className="m-0 px-3 text-sm text-faint">Tasks you start appear here.</p>}
            </div>
          </section>

          <section aria-label="Projects">
            <div className={SECTION}>Projects</div>
            <div className="stagger grid">
              {workspaces.slice(0, 6).map((workspace) => (
                <button
                  key={workspace.id}
                  type="button"
                  className={`${ROW} ${workspace.id === workspaceId ? "!bg-accent-soft" : ""}`}
                  onClick={() => uiActions.navigate({ kind: "new", workspaceId: workspace.id })}
                  title={workspace.path}
                >
                  <span className="min-w-0 flex-1 truncate text-[13px] font-medium">{workspace.name}</span>
                  <span className="w-12 shrink-0 text-right text-[11.5px] text-faint tabular-nums">{counts.get(workspace.id) ?? 0}</span>
                </button>
              ))}
              <button type="button" className={`${ROW} text-[13px] !text-muted hover:!text-accent`} onClick={() => setCreating(true)}>
                New project…
              </button>
            </div>
          </section>
        </div>
      </div>
      <NewWorkspaceModal opened={creating} onClose={() => setCreating(false)} />
    </div>
  );
}
