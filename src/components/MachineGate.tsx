import { Button } from "@mantine/core";
import { IconDeviceDesktopOff, IconDownload, IconPlayerPlay } from "@tabler/icons-react";
import type { ReactNode } from "react";
import type { MachineStatus } from "../services/ipc/generated/core";
import { machineIpc } from "../services/ipc/app";
import { useApp } from "../state/store";
import { uiActions } from "../state/ui";
import { attempt } from "../lib/operations";
import { EmptyState } from "./common";
import { BootHero, BootPage } from "./Boot";
import { StartReportView } from "./StartReportView";
import { RecoveryPanel } from "./Recovery";

function MachineSettingsButton() {
  return <Button variant="subtle" onClick={() => uiActions.navigate({ kind: "settings", tab: "machine" })}>Machine recovery</Button>;
}

/** Shown instead of a VM-backed view until cabind is connected. */
export function MachineGate({ children }: { children?: ReactNode }) {
  const guestReady = useApp((state) => state.guestReady);
  const machine = useApp((state) => state.machine);
  const start = useApp((state) => state.start);
  if (guestReady && start?.state !== "running" && start?.state !== "failed") return <>{children}</>;
  if (!machine) return <div className="page" />;

  // A start in progress (or one that just failed): show what it is doing.
  if (start && (start.state === "running" || (start.state === "failed" && !machine.busy && machine.error))) {
    return (
      <BootPage>
        <StartReportView key={start.startedAt} report={start}>
          {start.state === "failed" && (
            <div className="grid gap-3">
              {start.error && <RecoveryPanel message={start.error} compact retry={machine.state === "running"} />}
              <div className="flex flex-wrap justify-end gap-2.5">
                <MachineSettingsButton />
                <Button variant="default" onClick={() => uiActions.navigate({ kind: "install" })}>
                  Review installation
                </Button>
                <Button leftSection={<IconPlayerPlay size={16} />} onClick={() => attempt(machineIpc.start())}>
                  Try again
                </Button>
              </div>
            </div>
          )}
        </StartReportView>
      </BootPage>
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
                <Button variant="default" size="xs" onClick={() => uiActions.navigate({ kind: "install" })}>Show progress</Button>
              ) : undefined
            }
          />
      </BootPage>
    );
  }
  if (machine.state === "notInstalled") {
    return (
      <div className="page">
        <EmptyState
          icon={<IconDownload size={36} stroke={1.4} />}
          title="Install the machine first"
          action={<Button onClick={() => uiActions.navigate({ kind: "install" })}>Open the installation wizard</Button>}
        >
          Cabin runs Claude Code, Codex, OpenCode, Pi and Antigravity inside one Ubuntu VM on this computer. Setup is one screen and one Install button.
        </EmptyState>
      </div>
    );
  }
  if (machine.state === "stopped") {
    return (
      <div className="page">
        <EmptyState
          icon={<IconDeviceDesktopOff size={36} stroke={1.4} />}
          title="The machine is stopped"
          action={
            <div className="row">
              <Button leftSection={<IconPlayerPlay size={16} />} onClick={() => attempt(machineIpc.start())}>
                Start the machine
              </Button>
              <MachineSettingsButton />
            </div>
          }
        >
          Workspaces, sessions and agents live inside the VM. Start it to continue.
        </EmptyState>
        {machine.error && <RecoveryPanel compact message={machine.error} retry={false} />}
      </div>
    );
  }
  return (
    <BootPage>
      <BootHero
        tone={machine.error ? "bad" : "running"}
        progress={null}
        title={machine.connection === "updating" ? "Updating cabind…" : "Connecting to your machine…"}
        subtitle={
          machine.connection === "updating"
            ? "Installing the build that matches this app. Sessions resume after."
            : machine.error
              ? "The app will keep trying to connect."
              : "This usually takes a few seconds."
        }
      />
      {machine.error && (
        <div className="grid gap-3">
          <RecoveryPanel message={machine.error} />
          <div className="flex justify-end"><MachineSettingsButton /></div>
        </div>
      )}
    </BootPage>
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
