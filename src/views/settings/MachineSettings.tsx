import { useAutoRefresh } from "../../lib/useAutoRefresh";
import { Button } from "@mantine/core";
import { IconDownload, IconPlayerPlay, IconPlayerStop, IconServer, IconTrash } from "@tabler/icons-react";
import { useCallback, useEffect, useState } from "react";
import { CopyDiagnosticsButton, RecoveryPanel } from "../../components/Recovery";
import { toIpcError } from "../../services/ipc/common";
import { MachineDetailsPanel, MachineResources } from "./MachineDetailsPanel";
import { MachineResourceEditor } from "./MachineResourceEditor";
import { machineIpc } from "../../services/ipc/app";
import { confirmAction } from "../../services/ipc/dialogs";
import { useApp } from "../../state/store";
import { attempt } from "../../lib/operations";
import { uiActions } from "../../state/ui";
import { reportElapsed } from "../../lib/startReport";
import { formatBytes, formatDuration, formatInteger } from "../../lib/format";
import { machineLabel } from "../../components/MachineGate";
import { Section, StateIcon } from "../../components/common";
import { StartReportView } from "../../components/StartReportView";
import { SettingsPage } from "../SettingsView";
import { useI18n } from "../../i18n";

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

function useInspection<T>(load: () => Promise<T>, request: string | null, keepLast = false) {
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
  return request != null && (keepLast || result?.request === request) ? result : undefined;
}

const PAGES = [
  { value: "resources", labelKey: "machine.tab.resources" },
  { value: "details", labelKey: "machine.tab.details" },
  { value: "recovery", labelKey: "machine.tab.recovery" },
] as const;
type MachinePage = (typeof PAGES)[number]["value"];

export function MachineSettings() {
  const { t } = useI18n();
  const machine = useApp((state) => state.machine);
  const start = useApp((state) => state.start);
  const request = machine ? `${machine.state}:${machine.connection}:${machine.busy ?? ""}:${machine.display}:${machine.browser}:${machine.device}` : null;
  const installed = machine != null && machine.state !== "notInstalled";
  const connected = machine?.connection === "connected";
  const [chosen, setChosen] = useState<MachinePage | null>(null);
  const [resourceRevision, setResourceRevision] = useState(0);
  const inspection = useInspection(machineIpc.details, installed ? `${request}:${resourceRevision}` : null, true);
  const observation = useInspection(machineIpc.observation, connected ? request : null);
  const loadChecks = useCallback(
    () => machineIpc.checks(installed ? 0 : 20, machine?.memoryMb ?? 4096).then((report) => report.items),
    [installed, machine?.memoryMb],
  );
  const checks = useInspection(loadChecks, request);

  if (!machine) return <div className="page" />;

  const busy = machine.busy != null;
  const running = machine.state === "running";
  const blocking = checks?.value?.some((check) => !check.ok && !check.optional) ?? false;
  const tone = machine.error ? "bad" : connected && running ? "ok" : busy || running ? "warn" : "";
  const startFailed = start?.state === "failed";
  const recoveryNeeded = machine.error != null || startFailed;
  const cannotReadConfig = installed && inspection != null && !inspection.value;

  // A failure opens on the way out, until a section is chosen.
  const page = chosen ?? (startFailed ? "recovery" : "resources");

  const facts = [
    installed && machine.cpus != null ? `${formatInteger(machine.cpus)} vCPUs` : null,
    installed && machine.memoryMb != null ? `${formatBytes(machine.memoryMb * 1024 ** 2)} RAM` : null,
    installed && machine.diskSize ? `${machine.diskSize.replace(/^([\d.]+)([KMGT])$/i, "$1 $2iB")} disk` : null,
    machine.display ? "Desktop" : null,
    machine.browser ? "Chrome" : null,
    machine.device ? "Device profile" : null,
  ].filter((fact) => fact != null);

  return (
    <SettingsPage title={t("machine.title")}>
      <section className="mb-4 rounded-[var(--card-radius)] border border-line bg-surface p-5" aria-label={t("machine.title")}>
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
            <Button leftSection={<IconDownload size={14} />} onClick={() => uiActions.navigate({ kind: "install" })}>
              {busy ? t("gate.showProgress") : `${t("common.install")}…`}
            </Button>
          ) : running ? (
            <Button variant="default" leftSection={<IconPlayerStop size={14} />} loading={busy} disabled={busy} onClick={() => attempt(machineIpc.stop())}>{t("common.stop")}</Button>
          ) : (
            <Button leftSection={<IconPlayerPlay size={14} />} loading={busy} disabled={busy || cannotReadConfig} onClick={() => attempt(machineIpc.start())}>{t("common.start")}</Button>
          )}
        </div>
        {facts.length > 0 && (
          <div className="mt-4 flex flex-wrap gap-1.5">
            {facts.map((fact) => <span key={fact} className="rounded-full bg-hover px-2.5 py-1 text-xs font-medium text-muted">{fact}</span>)}
          </div>
        )}
      </section>

      {machine.error && <RecoveryPanel compact message={machine.error} />}
      <nav className="mb-4 flex flex-wrap gap-x-[18px] border-b border-line" aria-label={t("machine.title")}>
        {PAGES.map(({ value, labelKey }) => (
          <button key={value} type="button" aria-current={page === value ? "page" : undefined} onClick={() => setChosen(value)}
            className="press -mb-px cursor-pointer appearance-none border-0 border-b-2 border-solid border-transparent bg-transparent px-0.5 py-[9px] text-md font-semibold text-muted hover:text-text aria-[current=page]:border-accent aria-[current=page]:text-text">
            {t(labelKey)}
          </button>
        ))}
      </nav>
      {page === "resources" && <>
      {installed && inspection?.value && <MachineResourceEditor machine={machine} details={inspection.value}
        onSaved={() => setResourceRevision((revision) => revision + 1)} />}
        {installed ? <MachineResources machine={machine} observation={observation?.value ?? null} />
          : <p className="muted machine-note">{t("machine.installToConfigure")}</p>}
      {connected && observation?.error && <p className="muted machine-note" role="status">{observation.error}</p>}

      </>}
      {page === "details" && (inspection?.value && installed
        ? <MachineDetailsPanel machine={machine} details={inspection.value} observation={observation?.value ?? null} />
        : <p className="muted machine-note" role="status">{!installed ? t("machine.installToDetails") : inspection?.error ?? (inspection ? t("machine.configUnavailable") : t("machine.readingConfig"))}</p>)}
      {page === "recovery" && <>
      <Section title={t("machine.startupDiag")} action={<CopyDiagnosticsButton />}>
        {start ? (
          <details className="machine-expander" open={startFailed || start.state === "running" || undefined}>
            <summary>{startFailed ? t("machine.startFailed") : start.state === "running" ? t("machine.startInProgress") : t("machine.lastStart", { duration: formatDuration(reportElapsed(start, Date.now())) })}</summary>
            <StartReportView key={start.startedAt} report={start} />
          </details>
        ) : <p className="muted machine-note">{t("machine.noStartupReport")}</p>}
        <details className="machine-expander" open={blocking || undefined}>
          <summary>{t("machine.thisComputer")}</summary>
          {checks?.value?.length ? <ul className="machine-findings">
            {checks.value.map((check) => <li key={check.id}>
              <StateIcon tone={check.ok ? "ok" : check.optional ? "skipped" : "bad"} />
              <div><div>{check.label}</div><div className="muted fs-12">{check.detail}</div></div>
            </li>)}
          </ul> : <p className="muted machine-note">{checks?.error ?? (checks ? t("machine.noChecks") : t("machine.checkingComputer"))}</p>}
        </details>
      </Section>

      {(installed || recoveryNeeded) && <Section title={t("machine.recoveryMode")} hint={t("machine.recoveryHint")} quiet>
        {installed && <div className="mb-3 flex flex-wrap items-center gap-3">
          <div className="min-w-0 flex-1">
            <strong>{t("machine.verifyRepair")}</strong>
            <div className="text-sm text-muted">{t("machine.verifyRepairHint")}</div>
          </div>
          <Button variant="default" disabled={busy || cannotReadConfig} onClick={() => uiActions.navigate({ kind: "install" })}>{t("machine.reviewInstall")}</Button>
        </div>}
        <div className="danger-zone flex flex-wrap items-center gap-3">
          <div className="min-w-0 flex-1">
            <strong>{t("machine.eraseTitle")}</strong>
            <div className="text-sm text-muted">{t("machine.eraseHint")}</div>
          </div>
          <Button variant="light" color="red" leftSection={<IconTrash size={14} />} disabled={busy} onClick={() => void destroy()}>{t("machine.eraseBtn")}</Button>
        </div>
      </Section>}
      </>}
    </SettingsPage>
  );
}
