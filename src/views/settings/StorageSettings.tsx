import { useAutoRefresh } from "../../lib/useAutoRefresh";
import { Button, Progress } from "@mantine/core";
import { IconTrash } from "@tabler/icons-react";
import { useEffect, useRef, useState } from "react";
import { Section } from "../../components/common";
import { formatBytes } from "../../lib/format";
import { machineIpc, storageIpc } from "../../services/ipc/app";
import { toIpcError } from "../../services/ipc/common";
import { confirmAction } from "../../services/ipc/dialogs";
import type { CleanupTarget, StorageEntry, StorageKind, StorageReport } from "../../services/ipc/generated/common";
import type { GuestMachineObservation } from "../../services/ipc/generated/core";
import { useApp } from "../../state/store";
import { uiActions } from "../../state/ui";
import { SettingsPage } from "../SettingsView";

const labels: Record<StorageKind, [string, string]> = {
  application: ["Application", "Current Mac app bundle or development executable"],
  virtualDisk: ["Virtual disk", "Space the Ubuntu disk image occupies on the Mac"],
  guestFiles: ["Other VM files", "Firmware settings, guest setup files and console logs"],
  systemImages: ["Ubuntu images", "Downloaded system images used for setup and verification"],
  engine: ["QEMU engine", "Virtualization binaries and firmware"],
  downloads: ["Download cache", "Partial downloads, temporary extraction files and reusable archives"],
  logs: ["Diagnostic logs", "Startup, setup and shutdown records"],
  settings: ["Settings and sync", "Network settings, sync records and machine metadata"],
  webkitCache: ["WebKit cache", "Temporary content used by the Mac app window"],
  webkitData: ["WebKit site data", "Saved state used by the Mac app window"],
  otherData: ["Other app data", "Remaining data in the Mac app’s VM folder"],
  developerCache: ["Developer cache", "Local build outputs outside the installed app total"],
};

function BreakdownRow({ entry, capacity }: { entry: StorageEntry; capacity: string | null }) {
  const [label, description] = labels[entry.kind];
  return <div className="storage-row">
    <dt><strong>{label}</strong><span>{description}</span></dt>
    <dd><strong>{formatBytes(entry.bytes)}</strong>
      {entry.kind === "virtualDisk" && capacity && <span>{capacity.replace(/^([\d.]+)([KMGT])$/i, "$1 $2iB")} capacity</span>}
      {!entry.complete && <span>Partial scan</span>}
    </dd>
  </div>;
}

export function StorageSettings() {
  const state = useApp((app) => app.machine?.state);
  const connected = useApp((app) => app.machine?.connection === "connected");
  const busy = useApp((app) => app.machine?.busy != null);
  const [report, setReport] = useState<StorageReport | null>(null);
  const [guest, setGuest] = useState<GuestMachineObservation | null>(null);
  const [refresh, setRefresh] = useState(0);
  const [loading, setLoading] = useState(true);
  const [scanError, setScanError] = useState<string | null>(null);
  const [notice, setNotice] = useState<{ text: string; failed: boolean } | null>(null);
  const [cleaning, setCleaning] = useState<CleanupTarget | null>(null);
  const cleanupPending = useRef(false);
  useAutoRefresh(() => { if (!cleanupPending.current) setRefresh((value) => value + 1); }, { initial: false, minGap: 15_000 });

  useEffect(() => {
    let current = true;
    setLoading(true);
    setScanError(null);
    void storageIpc.status().then((value) => {
      if (current) setReport(value);
    }, (error: unknown) => {
      if (current) setScanError(toIpcError(error).message);
    }).finally(() => { if (current) setLoading(false); });
    return () => { current = false; };
  }, [refresh, state]);

  useEffect(() => {
    if (!connected) { setGuest(null); return; }
    let current = true;
    void machineIpc.observation().then((value) => { if (current) setGuest(value); }).catch(() => undefined);
    return () => { current = false; };
  }, [connected, refresh]);

  async function clean(target: CleanupTarget, bytes: number) {
    if (cleanupPending.current) return;
    cleanupPending.current = true;
    setNotice(null);
    try {
      const subject = target === "downloads" ? "download cache" : "old diagnostic logs";
      if (!await confirmAction(
        `Remove ${formatBytes(bytes)} of ${subject}? Your virtual disk, current system image and shared folders are kept.`,
        "Clean up", "Clean up storage",
      )) return;
      setCleaning(target);
      const result = await storageIpc.clean(target);
      setNotice({ failed: result.failures.length > 0, text: result.failures.length
        ? `Removed ${formatBytes(result.removedBytes)}. ${result.failures.join(" · ")}`
        : result.removedBytes ? `Removed ${formatBytes(result.removedBytes)} of ${subject}.` : "Nothing to remove." });
    } catch (error) {
      setNotice({ text: toIpcError(error).message, failed: true });
    } finally {
      setCleaning(null);
      setRefresh((value) => value + 1);
      cleanupPending.current = false;
    }
  }

  const volume = report?.volume;
  const dev = report?.entries.find((entry) => entry.kind === "developerCache");
  const runtimeEntries = report?.entries.filter((entry) => entry.kind !== "developerCache") ?? [];
  const used = volume?.totalBytes ? 100 * (1 - volume.availableBytes / volume.totalBytes) : null;
  const incomplete = runtimeEntries.some((entry) => !entry.complete);
  const blocked = state === "running" ? "Stop the machine to clean up storage" : report?.cleanupBlocked;
  const filesystem = connected ? guest : null;
  return <SettingsPage title="Storage">
    {scanError && <p className="callout bad" role="alert">{scanError}</p>}
    {loading && !report && <p className="muted" role="status">Measuring disk usage…</p>}
    {report && <>
      <div className="storage-summary">
        <div><span>App storage on Mac</span><strong>{incomplete ? "At least " : ""}{formatBytes(report.totalBytes)}</strong><span>App, machine and local data</span></div>
        <div><span>Available on the data volume</span><strong>{formatBytes(volume?.availableBytes)}</strong>
          {volume && <span>{formatBytes(Math.max(0, volume.totalBytes - volume.availableBytes))} used or reserved of {formatBytes(volume.totalBytes)}</span>}
          {used != null && <Progress value={Math.min(100, Math.max(0, used))} size="sm" aria-label="Data volume used or reserved" />}
        </div>
      </div>
      <Section title="Clean up">
        {blocked && <div className="row storage-blocked"><span className="muted">{blocked}</span>
          {state === "running" && <Button size="compact-sm" variant="subtle" onClick={() => uiActions.navigate({ kind: "settings", tab: "machine" })}>Machine settings</Button>}
        </div>}
        {notice && <div className={notice.failed ? "callout bad" : "storage-notice"} role={notice.failed ? "alert" : "status"}>{notice.text}</div>}
        <div className="storage-breakdown">
          {(["downloads", "logs"] as const).map((target) => {
            const bytes = runtimeEntries.find((entry) => entry.kind === target)?.cleanableBytes ?? 0;
            return <div className="storage-row" key={target}>
              <div><strong>{target === "downloads" ? "Download cache" : "Old diagnostic logs"}</strong>
                <span>{target === "downloads" ? "Completed QEMU archives and leftover download files" : `Logs older than ${report.logRetentionDays} days`}</span>
              </div>
              <div className="storage-clean-action">
                <span>{bytes ? `${formatBytes(bytes)} to clean` : "Nothing to clean"}</span>
                <Button variant="default" size="compact-sm" leftSection={<IconTrash size={14} />}
                  aria-label={target === "downloads" ? "Clean download cache" : "Clean old diagnostic logs"}
                  loading={cleaning === target} disabled={bytes === 0 || busy || blocked != null || loading || cleaning != null}
                  onClick={() => void clean(target, bytes)}>Clean up</Button>
              </div>
            </div>;
          })}
        </div>
      </Section>
      <Section title="Breakdown" hint="Sizes use allocated disk space. Space reclaimed can vary with the filesystem.">
        <dl className="storage-breakdown">
          {runtimeEntries.map((entry) => <BreakdownRow key={entry.kind} entry={entry} capacity={report.diskCapacity} />)}
        </dl>
        {filesystem?.rootAvailableBytes != null && filesystem.rootTotalBytes != null && <p className="muted fs-13">
          Ubuntu system disk · {formatBytes(filesystem.rootAvailableBytes)} available of {formatBytes(filesystem.rootTotalBytes)}
        </p>}
      </Section>
      <Section title="Locations">
        <dl className="storage-breakdown">
          <div className="storage-row"><dt>App data</dt><dd className="mono">{report.dataPath}</dd></div>
          {report.appPath && <div className="storage-row"><dt>Application</dt><dd className="mono">{report.appPath}</dd></div>}
          {dev && (dev.bytes > 0 || !dev.complete) && <BreakdownRow entry={dev} capacity={null} />}
        </dl>
      </Section>
    </>}
  </SettingsPage>;
}
