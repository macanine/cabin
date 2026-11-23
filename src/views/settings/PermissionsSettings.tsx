import { Badge } from "@mantine/core";
import { Section } from "../../components/common";
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
  const ordered = projects?.slice().sort((a, b) => Number(a.access.state === "available") - Number(b.access.state === "available"));

  return (
    <SettingsPage title="Folder permissions" subtitle="Access to the local folders you use for project sync.">
      <Section title="Project folders">
        {error && <p className="m-0 text-sm text-bad" role="alert">{error}</p>}
        {!projects && checking && <p className="m-0 text-sm text-muted" role="status">Checking access…</p>}
        {projects?.length === 0 && <p className="m-0 text-sm text-muted">No local folders are configured for project sync.</p>}
        {ordered?.map(({ id, name, path, needsWrite, access }) => (
          <div key={id} className="grid gap-2 border-b border-line/60 py-3 first:pt-0 last:border-b-0 last:pb-0">
            <div className="flex flex-wrap items-center justify-between gap-2">
              <strong className="min-w-0 text-md font-medium">{name}</strong>
              <Badge variant="light" color={access.state === "available" ? "teal" : "yellow"} size="sm">{ACCESS[access.state]}</Badge>
            </div>
            <div className="flex flex-wrap items-baseline justify-between gap-2 text-xs text-muted">
              <code className="min-w-0 [overflow-wrap:anywhere]">{path}</code>
              <span>{needsWrite ? "Read and write" : "Read"}</span>
            </div>
            {access.message && <FolderAccessHelp path={path} needsWrite={needsWrite} message={access.message}
              privacy={access.privacySettings} onCheck={() => void refreshMacPermissions()} />}
          </div>
        ))}
      </Section>
    </SettingsPage>
  );
}
