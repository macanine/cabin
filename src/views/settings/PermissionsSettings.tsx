import { SettingGroup, SettingRow } from "../../components/common";
import { FolderAccessHelp } from "../../components/sync/FolderAccessHelp";
import type { FolderAccessState } from "../../services/ipc/generated/common";
import { refreshMacPermissions, useMacPermissions } from "../../state/macPermissions";
import { SettingsPage } from "../SettingsView";

const ACCESS: Record<FolderAccessState, string> = {
  available: "Available", readDenied: "Read access needed", writeDenied: "Write access needed",
  missing: "Folder missing", unavailable: "Unavailable",
};

export function PermissionsSettings() {
  const { projects, checking, error } = useMacPermissions();
  const needs = projects?.filter(({ access }) => access.state !== "available") ?? [];
  const ready = projects?.filter(({ access }) => access.state === "available") ?? [];
  const group = (title: string, rows: typeof needs) => rows.length > 0 && (
    <SettingGroup title={title}>
      {rows.map(({ id, name, path, needsWrite, access }) => (
        <div key={id} className="set-row !grid !grid-cols-1 !gap-2.5">
          <div className="flex min-w-0 flex-wrap items-center gap-x-4 gap-y-1">
            <div className="set-row-copy"><strong>{name}</strong><span><code className="[overflow-wrap:anywhere]">{path}</code> · {needsWrite ? "Read and write" : "Read"}</span></div>
            <span className={`pill ${access.state === "available" ? "ok" : "warn"}`}>{ACCESS[access.state]}</span>
          </div>
          {access.message && <FolderAccessHelp path={path} needsWrite={needsWrite} message={access.message}
            privacy={access.privacySettings} onCheck={() => void refreshMacPermissions()} />}
        </div>
      ))}
    </SettingGroup>
  );

  return (
    <SettingsPage title="Folder permissions" subtitle="Access to the local folders you use for project sync.">
      {error && <p className="callout bad mt-0" role="alert">{error}</p>}
      {!projects && checking && <p className="m-0 text-sm text-muted" role="status">Checking access…</p>}
      {projects?.length === 0 && (
        <SettingGroup><SettingRow label="No project folders" hint="Folders you sync with a project appear here with their access." /></SettingGroup>
      )}
      {group("Needs access", needs)}
      {group("Available", ready)}
    </SettingsPage>
  );
}
