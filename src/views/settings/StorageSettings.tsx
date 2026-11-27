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
import { useI18n } from "../../i18n";

const KIND_KEYS = {
  application: ["storage.kind.application", "storage.kind.applicationDesc"],
  virtualDisk: ["storage.kind.virtualDisk", "storage.kind.virtualDiskDesc"],
  guestFiles: ["storage.kind.guestFiles", "storage.kind.guestFilesDesc"],
  systemImages: ["storage.kind.systemImages", "storage.kind.systemImagesDesc"],
  engine: ["storage.kind.engine", "storage.kind.engineDesc"],
  downloads: ["storage.kind.downloads", "storage.kind.downloadsDesc"],
  logs: ["storage.kind.logs", "storage.kind.logsDesc"],
  settings: ["storage.kind.settings", "storage.kind.settingsDesc"],
  webkitCache: ["storage.kind.webkitCache", "storage.kind.webkitCacheDesc"],
  webkitData: ["storage.kind.webkitData", "storage.kind.webkitDataDesc"],
  otherData: ["storage.kind.otherData", "storage.kind.otherDataDesc"],
  developerCache: ["storage.kind.developerCache", "storage.kind.developerCacheDesc"],
} as const satisfies Record<StorageKind, readonly [string, string]>;

function BreakdownRow({ entry, capacity }: { entry: StorageEntry; capacity: string | null }) {
  const { t } = useI18n();
  const [labelKey, descKey] = KIND_KEYS[entry.kind];
  return <div className="storage-row">
    <dt><strong>{t(labelKey)}</strong><span>{t(descKey)}</span></dt>
    <dd><strong>{formatBytes(entry.bytes)}</strong>
      {entry.kind === "virtualDisk" && capacity && <span>{capacity.replace(/^([\d.]+)([KMGT])$/i, "$1 $2iB")}</span>}
      {!entry.complete && <span>Partial scan</span>}
    </dd>
  </div>;
}

/** The entries as one bar, largest first. The list below names every size. */
function StorageBar({ entries }: { entries: StorageEntry[] }) {
  const { t } = useI18n();
  const sized = entries.filter((entry) => entry.bytes > 0).sort((a, b) => b.bytes - a.bytes);
  const total = sized.reduce((sum, entry) => sum + entry.bytes, 0);
  if (!total) return null;
  return (
    <div className="storage-bar" aria-hidden>
      {sized.map((entry, index) => (
        <span key={entry.kind} title={`${t(KIND_KEYS[entry.kind][0])} · ${formatBytes(entry.bytes)}`}
          style={{ flexGrow: entry.bytes / total, opacity: Math.max(0.22, 1 - index * 0.2) }} />
      ))}
    </div>
  );
}

export function StorageSettings() {
  const { t } = useI18n();
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
  const blocked = state === "running" ? t("storage.stopToClean") : report?.cleanupBlocked;
  const filesystem = connected ? guest : null;
  return <SettingsPage title={t("storage.title")}>
    {scanError && <p className="callout bad" role="alert">{scanError}</p>}
    {loading && !report && <p className="muted" role="status">{t("storage.measuring")}</p>}
    {report && <>
      <div className="storage-summary">
        <div><span>{t("storage.appStorage")}</span><strong>{incomplete ? "≥ " : ""}{formatBytes(report.totalBytes)}</strong><span>{t("storage.appStorageSub")}</span></div>
        <div><span>{t("storage.availableVolume")}</span><strong>{formatBytes(volume?.availableBytes)}</strong>
          {volume && <span>{t("storage.usedOf", { used: formatBytes(Math.max(0, volume.totalBytes - volume.availableBytes)), total: formatBytes(volume.totalBytes) })}</span>}
          {used != null && <Progress value={Math.min(100, Math.max(0, used))} size="sm" aria-label={t("storage.availableVolume")} />}
        </div>
      </div>
      <Section title={t("storage.cleanUp")}>
        {blocked && <div className="row storage-blocked"><span className="muted">{blocked}</span>
          {state === "running" && <Button size="compact-sm" variant="subtle" onClick={() => uiActions.navigate({ kind: "settings", tab: "machine" })}>{t("home.machineSettings")}</Button>}
        </div>}
        {notice && <div className={notice.failed ? "callout bad" : "storage-notice"} role={notice.failed ? "alert" : "status"}>{notice.text}</div>}
        <div className="storage-breakdown">
          {(["downloads", "logs"] as const).map((target) => {
            const bytes = runtimeEntries.find((entry) => entry.kind === target)?.cleanableBytes ?? 0;
            return <div className="storage-row" key={target}>
              <div><strong>{target === "downloads" ? t("storage.downloadCache") : t("storage.oldLogs")}</strong>
                <span>{target === "downloads" ? t("storage.downloadCacheHint") : t("storage.oldLogsHint", { days: report.logRetentionDays })}</span>
              </div>
              <div className="storage-clean-action">
                <span>{bytes ? t("storage.toClean", { size: formatBytes(bytes) }) : t("storage.nothingToClean")}</span>
                <Button variant="default" size="compact-sm" leftSection={<IconTrash size={14} />}
                  aria-label={target === "downloads" ? t("storage.downloadCache") : t("storage.oldLogs")}
                  loading={cleaning === target} disabled={bytes === 0 || busy || blocked != null || loading || cleaning != null}
                  onClick={() => void clean(target, bytes)}>{t("common.cleanUp")}</Button>
              </div>
            </div>;
          })}
        </div>
      </Section>
      <Section title={t("storage.whatTakesSpace")} hint={t("storage.whatTakesSpaceHint")}>
        <StorageBar entries={runtimeEntries} />
        <dl className="storage-breakdown">
          {runtimeEntries.map((entry) => <BreakdownRow key={entry.kind} entry={entry} capacity={report.diskCapacity} />)}
        </dl>
        {filesystem?.rootAvailableBytes != null && filesystem.rootTotalBytes != null && <p className="muted fs-13">
          {t("storage.ubuntuDisk", { avail: formatBytes(filesystem.rootAvailableBytes), total: formatBytes(filesystem.rootTotalBytes) })}
        </p>}
      </Section>
      <Section title={t("storage.locations")}>
        <dl className="storage-breakdown">
          <div className="storage-row"><dt>{t("storage.appData")}</dt><dd className="mono">{report.dataPath}</dd></div>
          {report.appPath && <div className="storage-row"><dt>{t("storage.application")}</dt><dd className="mono">{report.appPath}</dd></div>}
          {dev && (dev.bytes > 0 || !dev.complete) && <BreakdownRow entry={dev} capacity={null} />}
        </dl>
      </Section>
    </>}
  </SettingsPage>;
}
