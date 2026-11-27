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
import { t as translate, useI18n } from "../i18n";

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
  const { t } = useI18n();
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
          <Button variant="default" onClick={openMachineSettings}>{t("home.machineSettings")}</Button>
          <span className="flex-1" />
          <Button variant="default" onClick={() => uiActions.navigate({ kind: "install" })}>{t("gate.repairInstall")}</Button>
          <StartButton label={t("common.tryAgain")} busy={busy} />
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
          subtitle={t("gate.busyHint")}
          aside={
            machine.busy === "Installing the machine" ? (
              <Button className="mt-2" variant="default" size="xs" onClick={() => uiActions.navigate({ kind: "install" })}>{t("gate.showProgress")}</Button>
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
        title={t("gate.setupTitle")}
        lead={t("gate.setupLead")}
        main={<div><Button size="md" rightSection={<IconArrowRight size={16} />} onClick={() => uiActions.navigate({ kind: "install" })}>{t("gate.setupBtn")}</Button></div>}
      />
    );
  }
  if (machine.state === "stopped") {
    return (
      <MachineStage
        center
        title={t("gate.stoppedTitle")}
        lead={t("gate.stoppedLead")}
        main={<>
          {machine.error && <RecoveryPanel compact message={machine.error} retry={false} />}
          <div className="flex flex-wrap items-center gap-2">
            <StartButton label={t("nav.startMachine")} busy={busy} />
            <Button size="md" variant="default" onClick={openMachineSettings}>{t("home.machineSettings")}</Button>
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
      title={updating ? t("gate.updatingTitle") : t("gate.connectingTitle")}
      lead={updating
        ? t("gate.updatingLead")
        : machine.error ? t("gate.connectingRetry") : t("gate.connectingLead")}
      main={machine.error
        ? <RecoveryPanel message={machine.error} />
        : <div className="stage-progress" data-indeterminate role="progressbar" aria-label={updating ? t("gate.status.updating") : t("gate.status.connecting")}><span /></div>}
      aside={<Card machine={machine} tone={machine.error ? "failed" : "starting"} screen={updating ? "updating cabind…" : "waiting for cabind…"} />}
      bar={machine.error ? <><CopyDiagnosticsButton /><span className="flex-1" /><Button variant="default" onClick={openMachineSettings}>{t("home.machineSettings")}</Button></> : undefined}
    />
  );
}

export function machineLabel(state: MachineStatus | null): string {
  if (!state) return "…";
  if (state.busy) return state.busy;
  switch (state.state) {
    case "notInstalled":
      return translate("gate.status.notInstalled");
    case "stopped":
      return translate("gate.status.stopped");
    case "running":
      return state.connection === "connected"
        ? translate("gate.status.running")
        : state.connection === "updating"
          ? translate("gate.status.updating")
          : translate("gate.status.connecting");
  }
}
