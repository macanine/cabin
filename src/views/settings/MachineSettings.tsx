import { useAutoRefresh } from "../../lib/useAutoRefresh";
import { Button } from "@mantine/core";
import { IconDownload, IconPlayerPlay, IconPlayerStop, IconServer, IconTrash } from "@tabler/icons-react";
import { useEffect, useState } from "react";
import { RecoveryPanel } from "../../components/Recovery";
import { toIpcError } from "../../services/ipc/common";
import { MachineDetailsPanel, MachineResources } from "./MachineDetailsPanel";
import { machineIpc } from "../../services/ipc/app";
import { confirmAction } from "../../services/ipc/dialogs";
import { useApp } from "../../state/store";
import { attempt } from "../../lib/operations";
import { uiActions } from "../../state/ui";
import { isOptionalCheck } from "../../lib/installForm";
import { reportElapsed } from "../../lib/startReport";
import { formatBytes, formatDuration, formatInteger } from "../../lib/format";
import { machineLabel } from "../../components/MachineGate";
import { Section, StateIcon } from "../../components/common";
import { StartReportView } from "../../components/StartReportView";
import { SettingsPage } from "../SettingsView";

async function destroy() {
  if (!await confirmAction(
    "Permanently erase the VM disk, workspaces, sessions, downloaded images, logs and app settings? Shared folders on your Mac are kept. Setup opens again after the reset.",
    "Erase machine",
    "Erase and reinstall",
  )) return;
  const result = await attempt(machineIpc.destroy());
  if (!result) return;
  try {
    const keys = Array.from({ length: localStorage.length }, (_, index) => localStorage.key(index));
    for (const key of keys) if (key?.startsWith("cabin.")) localStorage.removeItem(key);
  } finally {
    window.location.reload();
  }
}

function useInspection<T>(load: () => Promise<T>, request: string | null) {
  const [result, setResult] = useState<{ request: string; value?: T; error?: string }>();
  const [epoch, setEpoch] = useState(0);
  useAutoRefresh(() => setEpoch((value) => value + 1), { enabled: request != null, initial: false, minGap: 5_000 });
  useEffect(() => {
    if (request == null) return;
    let current = true;
    void load().then((value) => {
      if (current) setResult({ request, value });
    }, (error: unknown) => {
      if (current) setResult((last) => last?.request === request && last.value != null ? last : { request, error: toIpcError(error).message });
    });
    return () => { current = false; };
  }, [load, request, epoch]);
  return request != null && result?.request === request ? result : undefined;
}

export function MachineSettings() {
  const machine = useApp((state) => state.machine);
  const start = useApp((state) => state.start);
  const request = machine ? `${machine.state}:${machine.connection}:${machine.busy ?? ""}:${machine.display}:${machine.browser}:${machine.device}` : null;
  const installed = machine != null && machine.state !== "notInstalled";
  const connected = machine?.connection === "connected";
  const inspection = useInspection(machineIpc.details, installed ? request : null);
  const observation = useInspection(machineIpc.observation, connected ? request : null);
  const checks = useInspection(machineIpc.checks, request);

  if (!machine) return <div className="page" />;

  const busy = machine.busy != null;
  const running = machine.state === "running";
  const blocking = checks?.value?.some((check) => !check.ok && !isOptionalCheck(check.id)) ?? false;
  const tone = machine.error ? "bad" : connected && running ? "ok" : busy || running ? "warn" : "";
  const startFailed = start?.state === "failed";
  const recoveryNeeded = machine.error != null || startFailed;
  const cannotReadConfig = installed && inspection != null && !inspection.value;

  const facts = [
    installed && machine.cpus != null ? `${formatInteger(machine.cpus)} vCPUs` : null,
    installed && machine.memoryMb != null ? `${formatBytes(machine.memoryMb * 1024 ** 2)} RAM` : null,
    installed && machine.diskSize ? `${machine.diskSize.replace(/^([\d.]+)([KMGT])$/i, "$1 $2iB")} disk` : null,
    machine.display ? "Desktop" : null,
    machine.browser ? "Chrome" : null,
    machine.device ? "Device profile" : null,
  ].filter((fact) => fact != null);

  return (
    <SettingsPage title="Machine">
      <section className="glass mb-4 rounded-2xl p-5" aria-label="Machine status">
        <div className="flex flex-wrap items-center gap-4">
          <span className={`relative grid size-12 shrink-0 place-items-center rounded-2xl ${tone === "ok" ? "bg-ok/13 text-ok" : tone === "warn" ? "bg-warn/14 text-warn" : tone === "bad" ? "bg-bad/13 text-bad" : "bg-hover text-muted"}`}>
            <IconServer size={22} />
            <span className={`absolute -top-0.5 -right-0.5 size-3 rounded-full border-2 border-surface ${tone === "ok" ? "bg-ok" : tone === "warn" ? "bg-warn" : tone === "bad" ? "bg-bad" : "bg-faint"}`} />
          </span>
          <div className="min-w-0 flex-1">
            <div className="flex flex-wrap items-baseline gap-x-2">
              <strong className="text-lg font-semibold [overflow-wrap:anywhere]">{machine.hostname ?? "Ubuntu machine"}</strong>
              <span className="text-md text-muted" role="status">{machineLabel(machine)}</span>
            </div>
            {machine.guestVersion && <div className="text-xs text-faint">cabind {machine.guestVersion}{machine.os ? ` · ${machine.os}` : ""}</div>}
          </div>
          {!installed ? (
            <Button leftSection={<IconDownload size={14} />} disabled={!busy && blocking} onClick={() => uiActions.navigate({ kind: "install" })}>
              {busy ? "Show progress" : "Install…"}
            </Button>
          ) : running ? (
            <Button variant="default" leftSection={<IconPlayerStop size={14} />} loading={busy} disabled={busy} onClick={() => attempt(machineIpc.stop(), "Machine stopped")}>Stop</Button>
          ) : (
            <Button leftSection={<IconPlayerPlay size={14} />} loading={busy} disabled={busy || cannotReadConfig} onClick={() => attempt(machineIpc.start(), "Machine started")}>Start</Button>
          )}
        </div>
        {facts.length > 0 && (
          <div className="mt-4 flex flex-wrap gap-1.5">
            {facts.map((fact) => <span key={fact} className="rounded-full bg-hover px-2.5 py-1 text-xs font-medium text-muted">{fact}</span>)}
          </div>
        )}
      </section>

      {machine.error && <RecoveryPanel compact message={machine.error} />}
      {installed && (inspection?.value ? (
        <MachineDetailsPanel machine={machine} details={inspection.value} observation={observation?.value ?? null} />
      ) : (
        <>
          <p className="muted" role="status">{inspection?.error ?? (inspection ? "Saved machine configuration is unavailable" : "Reading machine configuration…")}</p>
          <MachineResources machine={machine} observation={observation?.value ?? null} />
        </>
      ))}
      {connected && observation?.error && <p className="muted machine-note" role="status">Guest observation unavailable · {observation.error}</p>}

      <Section title="Startup and diagnostics">
        {start ? (
          <details className="machine-expander" open={startFailed || start.state === "running" || undefined}>
            <summary>{startFailed ? "Start failed" : start.state === "running" ? "Start in progress" : `Last start · ${formatDuration(reportElapsed(start, Date.now()))}`}</summary>
            <StartReportView key={start.startedAt} report={start} compact />
          </details>
        ) : <p className="muted machine-note">No startup report in this app session</p>}
        <details className="machine-expander" open={blocking || undefined}>
          <summary>This Mac</summary>
          {checks?.value?.length ? <ul className="machine-findings">
            {checks.value.map((check) => <li key={check.id}>
              <StateIcon tone={check.ok ? "ok" : isOptionalCheck(check.id) ? "skipped" : "bad"} />
              <div><div>{check.label}</div><div className="muted fs-12">{check.detail}</div></div>
            </li>)}
          </ul> : <p className="muted machine-note">{checks?.error ?? (checks ? "No checks available" : "Checking this Mac…")}</p>}
        </details>
      </Section>

      {(installed || recoveryNeeded) && <Section title="Recovery mode" hint="These tools run on your Mac, even when the guest cannot connect." quiet>
        {installed && <div className="mb-3 flex flex-wrap items-center gap-3">
          <div className="min-w-0 flex-1">
            <strong>Verify or repair installation</strong>
            <div className="text-sm text-muted">Review the recorded setup steps and resume repair with the machine stopped.</div>
          </div>
          <Button variant="default" disabled={busy || cannotReadConfig} onClick={() => uiActions.navigate({ kind: "install" })}>Review installation</Button>
        </div>}
        <div className="danger-zone flex flex-wrap items-center gap-3">
          <div className="min-w-0 flex-1">
            <strong>Erase and reinstall</strong>
            <div className="text-sm text-muted">Deletes the VM disk and app data, then opens setup. Shared folders on your Mac are kept.</div>
          </div>
          <Button variant="light" color="red" leftSection={<IconTrash size={14} />} disabled={busy} onClick={() => void destroy()}>Erase machine…</Button>
        </div>
      </Section>}
    </SettingsPage>
  );
}
