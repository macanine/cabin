import { Button } from "@mantine/core";
import { IconArrowRight, IconPlayerPlay } from "@tabler/icons-react";
import type { ReactNode } from "react";
import type { MachineStatus } from "../services/ipc/generated/core";
import { machineIpc } from "../services/ipc/app";
import { useApp } from "../state/store";
import { uiActions } from "../state/ui";
import { diskGb } from "../lib/installForm";
import { attempt } from "../lib/operations";
import { type CardTone, MachineCard } from "../views/install/MachineCard";
import { BootHero, BootPage } from "./Boot";
import { MachineStage } from "./MachineStage";
import { StartStage } from "./StartReportView";
import { CopyDiagnosticsButton, RecoveryPanel } from "./Recovery";

const openMachineSettings = () => uiActions.navigate({ kind: "settings", tab: "machine" });

/** "ubuntu-24.04" → "Ubuntu 24.04 LTS", without asking a machine that may be down. */
function osName(os: string | null | undefined): string {
  const version = /^ubuntu-(.+)$/.exec(os ?? "")?.[1];
  return version ? `Ubuntu ${version} LTS` : "Your machine";
}

function Card({ machine, tone, screen }: { machine: MachineStatus; tone: CardTone; screen?: string | null }) {
  return (
    <MachineCard host={null} tone={tone} screen={screen} spec={{
      osName: osName(machine.os),
      cpus: machine.cpus,
      memoryMb: machine.memoryMb,
      diskGb: diskGb(machine.diskSize),
      username: machine.username ?? "dev",
      hostname: machine.hostname ?? "dev-vm",
    }} />
  );
}

const StartButton = ({ label, busy }: { label: string; busy: boolean }) => (
  <Button size="md" leftSection={<IconPlayerPlay size={16} />} disabled={busy} onClick={() => attempt(machineIpc.start())}>{label}</Button>
);

/** Shown instead of a VM-backed view until cabind is connected. */
export function MachineGate({ children }: { children?: ReactNode }) {
  const guestReady = useApp((state) => state.guestReady);
  const machine = useApp((state) => state.machine);
  const start = useApp((state) => state.start);
  if (guestReady && start?.state !== "running" && start?.state !== "failed") return <>{children}</>;
  if (!machine) return <div className="page" />;
  const busy = machine.busy != null;

  // A start in progress (or one that just failed): show what it is doing.
  if (start && (start.state === "running" || (start.state === "failed" && !busy && machine.error))) {
    const failed = start.state === "failed";
    return (
      <StartStage
        key={start.startedAt}
        report={start}
        card={(screen) => <Card machine={machine} tone={failed ? "failed" : "starting"} screen={screen} />}
        recovery={failed && start.error ? <RecoveryPanel message={start.error} compact retry={machine.state === "running"} /> : undefined}
        actions={failed ? <>
          <CopyDiagnosticsButton />
          <Button variant="default" onClick={openMachineSettings}>Machine settings</Button>
          <span className="flex-1" />
          <Button variant="default" onClick={() => uiActions.navigate({ kind: "install" })}>Repair installation</Button>
          <StartButton label="Try again" busy={busy} />
        </> : undefined}
      />
    );
  }

  if (machine.busy) {
    return (
      <BootPage>
        <BootHero
          tone="running"
          progress={null}
          title={`${machine.busy}…`}
          subtitle="This can take a few minutes."
          aside={
            machine.busy === "Installing the machine" ? (
              <Button className="mt-2" variant="default" size="xs" onClick={() => uiActions.navigate({ kind: "install" })}>Show progress</Button>
            ) : undefined
          }
        />
      </BootPage>
    );
  }
  if (machine.state === "notInstalled") {
    return (
      <MachineStage
        center
        title="Set up your agent machine"
        lead="Claude Code, Codex, OpenCode, Pi and Antigravity run inside one private Ubuntu machine on this computer."
        main={<div><Button size="md" rightSection={<IconArrowRight size={16} />} onClick={() => uiActions.navigate({ kind: "install" })}>Set up the machine</Button></div>}
      />
    );
  }
  if (machine.state === "stopped") {
    return (
      <MachineStage
        center
        title="Your machine is stopped"
        lead="Workspaces, tasks and agents are saved on its disk. Start it to pick up where you left off."
        main={<>
          {machine.error && <RecoveryPanel compact message={machine.error} retry={false} />}
          <div className="flex flex-wrap items-center gap-2">
            <StartButton label="Start machine" busy={busy} />
            <Button size="md" variant="default" onClick={openMachineSettings}>Machine settings</Button>
          </div>
        </>}
        aside={<Card machine={machine} tone={machine.error ? "failed" : "stopped"} />}
      />
    );
  }
  const updating = machine.connection === "updating";
  return (
    <MachineStage
      center
      title={updating ? "Updating Cabin's service" : "Connecting to your machine"}
      lead={updating
        ? "Installing the build that matches this app. Sessions resume after."
        : machine.error ? "The app will keep trying to connect." : "This usually takes a few seconds."}
      main={machine.error
        ? <RecoveryPanel message={machine.error} />
        : <div className="stage-progress" data-indeterminate role="progressbar" aria-label={updating ? "Updating" : "Connecting"}><span /></div>}
      aside={<Card machine={machine} tone={machine.error ? "failed" : "starting"} screen={updating ? "updating cabind…" : "waiting for cabind…"} />}
      bar={machine.error ? <><CopyDiagnosticsButton /><span className="flex-1" /><Button variant="default" onClick={openMachineSettings}>Machine settings</Button></> : undefined}
    />
  );
}

export function machineLabel(state: MachineStatus | null): string {
  if (!state) return "…";
  if (state.busy) return state.busy;
  switch (state.state) {
    case "notInstalled":
      return "Not installed";
    case "stopped":
      return "Stopped";
    case "running":
      return state.connection === "connected" ? "Running" : state.connection === "updating" ? "Updating agent" : "Connecting…";
  }
}
