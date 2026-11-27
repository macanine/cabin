import { Button, Loader, Menu, Tooltip } from "@mantine/core";
import { IconPlayerPlay, IconPlayerStop, IconSettings, IconRobot, IconServer, IconShieldLock } from "@tabler/icons-react";
import { useState, type ComponentProps } from "react";
import { machineIpc } from "../services/ipc/app";
import { useApp } from "../state/store";
import { uiActions } from "../state/ui";
import { attempt } from "../lib/operations";
import { attentionCount } from "../lib/agentsHub";
import { networkGuidance, selectNetworkSummary } from "../lib/networkSummary";
import { AGENT_KINDS } from "../components/AgentPicker";
import { CabinMark } from "../components/CabinMark";
import { Composer } from "../components/Composer";
import { RecentTasks } from "../components/RecentTasks";
import { useI18n } from "../i18n";

type Tone = "ok" | "warn" | "bad" | "off";
const DOT: Record<Tone, string> = { ok: "bg-ok", warn: "bg-warn", bad: "bg-bad", off: "bg-faint" };

/** One live fact about the setup: compact centered chip with full detail on hover. */
const Fact = ({ tone, label, detail, ...rest }: { tone: Tone; label: string; detail?: string | null } & ComponentProps<"button">) => {
  const button = (
    <button
      type="button"
      {...rest}
      className="press inline-flex h-7 max-w-[260px] min-w-0 shrink cursor-pointer items-center gap-1.5 rounded-full border border-line/65 bg-surface/65 px-2.5 text-left text-xs text-muted hover:border-line hover:bg-surface hover:text-fg data-expanded:border-line data-expanded:bg-surface"
    >
      <span className={`size-1.5 shrink-0 rounded-full ${DOT[tone]}`} />
      <span className="shrink-0 font-medium">{label}</span>
      {detail && <span className="min-w-0 truncate text-faint">{detail}</span>}
    </button>
  );
  return detail && detail.length > 28 ? <Tooltip label={detail} openDelay={250} multiline maw={340}>{button}</Tooltip> : button;
};

export function NewSessionView({ workspaceId }: { workspaceId?: string }) {
  const { t } = useI18n();
  const machine = useApp((state) => state.machine);
  const networkStatus = useApp((state) => state.networkLoaded ? state.network : null);
  const network = useApp(selectNetworkSummary);
  const agents = useApp((state) => state.agents);
  const attention = useApp((state) => attentionCount(state.agents));
  const [submitting, setSubmitting] = useState(false);
  const noAgents = agents.length > 0 && agents.every((agent) => !agent.installed);
  const ready = agents.filter((agent) => agent.installed).length;
  const running = machine?.state === "running";
  const stopped = machine?.state === "stopped";
  const holding = running && networkStatus?.connectivity === "rechecking";
  const blockedGuidance = running && (network.tone === "bad" || holding) ? networkGuidance(networkStatus, network) : null;
  const machineLine = machine
    ? [machine.cpus ? `${machine.cpus} CPU` : null, machine.memoryMb ? `${Math.round(machine.memoryMb / 1024)} GB` : null].filter(Boolean).join(" · ")
    : null;

  return (
    <div className="home-scroll">
      <div className="home-stage">
        <div className={`mb-5 flex flex-col items-center gap-2.5 transition-opacity duration-200 ${submitting ? "opacity-35" : ""}`}>
          <CabinMark variant="color" size={34} className="text-fg" />
          <h1 className="m-0 text-center text-[28px] font-semibold tracking-[-0.025em] text-fg">
            {t("home.title")}
          </h1>
        </div>

        {stopped || machine?.state === "notInstalled" ? (
          <div className="glass flex items-center gap-3 rounded-2xl p-4 text-md">
            <IconServer size={20} className="shrink-0 text-warn" />
            <span className="min-w-0 flex-1">{stopped ? t("home.machineStopped") : t("home.machineNotInstalled")}</span>
            {stopped
              ? <Button size="xs" leftSection={<IconPlayerPlay size={14} />} onClick={() => attempt(machineIpc.start())}>{t("home.resumeMachine")}</Button>
              : <Button size="xs" onClick={() => uiActions.navigate({ kind: "install" })}>{t("common.install")}</Button>}
          </div>
        ) : noAgents ? (
          <div className="glass flex items-center gap-3 rounded-2xl p-4 text-md">
            <IconRobot size={20} className="shrink-0 text-warn" />
            <span className="min-w-0 flex-1">{t("home.noAgents")}</span>
            <Button size="xs" onClick={() => uiActions.navigate({ kind: "agents" })}>{t("home.installAgents")}</Button>
          </div>
        ) : (
          <Composer key={workspaceId ?? "any"} workspaceId={workspaceId} onSubmittingChange={setSubmitting} />
        )}
        <div className={`mt-3.5 flex min-w-0 flex-wrap items-center justify-center gap-1.5 transition-opacity duration-200 ${submitting ? "opacity-35" : ""}`}>
          <Menu position="bottom" withinPortal shadow="md" width={220}>
            <Menu.Target>
              <Fact
                tone={machine?.busy ? "warn" : running ? (machine?.connection === "connected" ? "ok" : "warn") : stopped ? "off" : "bad"}
                label={machine?.busy ? machine.busy : running ? (machine?.connection === "connected" ? t("home.machineRunning") : t("home.connecting")) : stopped ? t("home.machineStoppedShort") : t("home.machineNotInstalledShort")}
                detail={running ? machineLine : null}
              />
            </Menu.Target>
            <Menu.Dropdown>
              {running && <Menu.Item leftSection={<IconPlayerStop size={14} />} disabled={machine?.busy != null} onClick={() => attempt(machineIpc.stop())}>{t("nav.stopMachine")}</Menu.Item>}
              {stopped && <Menu.Item leftSection={<IconPlayerPlay size={14} />} disabled={machine?.busy != null} onClick={() => attempt(machineIpc.start())}>{t("home.resumeMachine")}</Menu.Item>}
              <Menu.Item leftSection={<IconSettings size={14} />} onClick={() => uiActions.navigate({ kind: "settings", tab: "machine" })}>{t("home.machineSettings")}</Menu.Item>
            </Menu.Dropdown>
          </Menu>
          <Fact tone={network.tone === "ok" ? "ok" : network.tone === "bad" ? "bad" : "warn"} label={holding ? t("network.holding") : network.title} detail={network.tone === "ok" ? null : network.detail} onClick={uiActions.openNetworkCheck} />
          <Fact tone={attention > 0 ? "warn" : ready > 0 ? "ok" : "bad"} label={t("home.agentsSummary", { ready, total: AGENT_KINDS.length })} detail={attention > 0 ? t("home.needAttention", { count: attention }) : null} onClick={() => uiActions.navigate({ kind: "agents" })} />
        </div>
        {blockedGuidance && (
          <div className={`mt-4 flex flex-wrap items-center gap-3 rounded-2xl border px-4 py-3 text-sm transition-opacity duration-200 ${holding ? "border-info/25 bg-info/[0.06]" : "border-bad/25 bg-bad/[0.05]"} ${submitting ? "opacity-30" : ""}`} role={holding ? "status" : "alert"}>
            <span className={`grid size-7 shrink-0 place-items-center rounded-lg ${holding ? "bg-info/12 text-info" : "bg-bad/12 text-bad"}`} aria-hidden>
              {holding ? <Loader size={14} /> : <IconShieldLock size={16} />}
            </span>
            <div className="min-w-0 flex-1">
              <strong className="block text-[13px] font-semibold text-fg">{blockedGuidance.headline}</strong>
              <span className="block text-xs text-muted">
                {holding ? [network.detail, networkStatus?.connectivityError].filter(Boolean).join(" · ") || blockedGuidance.explanation : blockedGuidance.explanation}
              </span>
            </div>
            <Button size="xs" variant={holding ? "default" : "filled"} onClick={uiActions.openNetworkCheck}>{holding ? t("common.inspect") : t("common.resolve")}</Button>
          </div>
        )}
        <div className={`transition-opacity duration-200 ${submitting ? "opacity-30" : ""}`}>
          <RecentTasks />
        </div>
      </div>
    </div>
  );
}
