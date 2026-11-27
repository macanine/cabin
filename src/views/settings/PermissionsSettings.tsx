import { SettingGroup, SettingRow } from "../../components/common";
import { FolderAccessHelp } from "../../components/sync/FolderAccessHelp";
import type { FolderAccessState } from "../../services/ipc/generated/common";
import { refreshMacPermissions, useMacPermissions } from "../../state/macPermissions";
import { SettingsPage } from "../SettingsView";
import { type MessageKey, useI18n } from "../../i18n";

const ACCESS_KEY: Record<FolderAccessState, MessageKey> = {
  available: "perm.state.available",
  readDenied: "perm.state.readDenied",
  writeDenied: "perm.state.writeDenied",
  missing: "perm.state.missing",
  unavailable: "perm.state.unavailable",
};

export function PermissionsSettings() {
  const { t } = useI18n();
  const { projects, checking, error } = useMacPermissions();
  const needs = projects?.filter(({ access }) => access.state !== "available") ?? [];
  const ready = projects?.filter(({ access }) => access.state === "available") ?? [];
  const group = (title: string, rows: typeof needs) => rows.length > 0 && (
    <SettingGroup title={title}>
      {rows.map(({ id, name, path, needsWrite, access }) => (
        <div key={id} className="set-row !grid !grid-cols-1 !gap-2.5">
          <div className="flex min-w-0 flex-wrap items-center gap-x-4 gap-y-1">
            <div className="set-row-copy"><strong>{name}</strong><span><code className="[overflow-wrap:anywhere]">{path}</code> · {needsWrite ? t("perm.readWrite") : t("perm.readOnly")}</span></div>
            <span className={`pill ${access.state === "available" ? "ok" : "warn"}`}>{t(ACCESS_KEY[access.state] as "perm.state.available")}</span>
          </div>
          {access.message && <FolderAccessHelp path={path} needsWrite={needsWrite} message={access.message}
            privacy={access.privacySettings} onCheck={() => void refreshMacPermissions()} />}
        </div>
      ))}
    </SettingGroup>
  );

  return (
    <SettingsPage title={t("perm.title")} subtitle={t("perm.subtitle")}>
      {error && <p className="callout bad mt-0" role="alert">{error}</p>}
      {!projects && checking && <p className="m-0 text-sm text-muted" role="status">{t("perm.checking")}</p>}
      {projects?.length === 0 && (
        <SettingGroup><SettingRow label={t("perm.empty")} hint={t("perm.emptyHint")} /></SettingGroup>
      )}
      {group(t("perm.needsAccess"), needs)}
      {group(t("perm.available"), ready)}
    </SettingsPage>
  );
}
