import { Button, Menu } from "@mantine/core";
import { IconPlayerPlay, IconPlayerStop, IconSettings, IconRobot, IconServer } from "@tabler/icons-react";
import type { ComponentProps } from "react";
import { machineIpc } from "../services/ipc/app";
import { useApp } from "../state/store";
import { uiActions } from "../state/ui";
import { attempt } from "../lib/operations";
import { attentionCount } from "../lib/agentsHub";
import { selectNetworkSummary } from "../lib/networkSummary";
import { AGENT_KINDS } from "../components/AgentPicker";
import { Composer } from "../components/Composer";
import { RecentTasks } from "../components/RecentTasks";

type Tone = "ok" | "warn" | "bad" | "off";
const DOT: Record<Tone, string> = { ok: "bg-ok", warn: "bg-warn", bad: "bg-bad", off: "bg-faint" };

/** One live fact about the setup: a status dot, a label, an optional quiet detail. */
const Fact = ({ tone, label, detail, ...rest }: { tone: Tone; label: string; detail?: string | null } & ComponentProps<"button">) => (
    <button type="button" {...rest} className="press inline-flex h-7 min-w-0 shrink cursor-pointer items-center gap-2 rounded-lg border-0 bg-transparent px-2 text-left text-xs text-muted hover:text-fg hover:bg-hover data-expanded:bg-hover">
      <span className={`size-1.5 shrink-0 rounded-full ${DOT[tone]}`} />
      <span className="shrink-0 font-medium">{label}</span>
      {detail && <span className="min-w-0 truncate text-faint">{detail}</span>}
    </button>
);

export function NewSessionView({ workspaceId }: { workspaceId?: string }) {
  const machine = useApp((state) => state.machine);
  const network = useApp(selectNetworkSummary);
  const agents = useApp((state) => state.agents);
  const attention = useApp((state) => attentionCount(state.agents));
  const noAgents = agents.length > 0 && agents.every((agent) => !agent.installed);
  const ready = agents.filter((agent) => agent.installed).length;
  const running = machine?.state === "running";
  const stopped = machine?.state === "stopped";
  const machineLine = machine
    ? [machine.cpus ? `${machine.cpus} CPU` : null, machine.memoryMb ? `${Math.round(machine.memoryMb / 1024)} GB` : null].filter(Boolean).join(" · ")
    : null;

  return (
    <div className="min-h-0 flex-1 overflow-y-auto px-[clamp(16px,4vw,36px)] pb-12">
      <div className="mx-auto w-full max-w-[760px]">
        <h1 className="mt-[clamp(28px,11vh,120px)] mb-5 text-[26px] font-semibold tracking-tight">What should an agent work on?</h1>

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
        <div className="mt-3 flex min-w-0 flex-wrap items-center gap-x-1 gap-y-0.5">
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
        <RecentTasks />
      </div>
    </div>
  );
}
