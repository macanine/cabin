import { Button } from "@mantine/core";
import { useState } from "react";
import { toIpcError } from "../../services/ipc/common";
import { macPermissionsIpc } from "../../services/ipc/permissions";

/** Shared by permission settings and the folder chooser when access fails. */
export function FolderAccessHelp({ path, needsWrite, message, privacy, onCheck }: {
  path: string;
  needsWrite: boolean;
  message: string;
  privacy: boolean;
  onCheck: () => void;
}) {
  const [requesting, setRequesting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function openSettings() {
    setError(null);
    try {
      await macPermissionsIpc.openFileSettings();
    } catch (cause) {
      setError(toIpcError(cause).message);
    }
  }

  async function request() {
    setError(null);
    setRequesting(true);
    try {
      await macPermissionsIpc.checkFolder(path, needsWrite, true);
      onCheck();
    } catch (cause) {
      setError(toIpcError(cause).message);
    } finally {
      setRequesting(false);
    }
  }

  return (
    <div className="grid gap-2 text-sm" role="status">
      <div className="text-warn">{message}</div>
      {privacy && <div className="text-muted">If macOS has already asked, allow Cabin in Privacy &amp; Security → Files and Folders.</div>}
      {privacy && <div className="flex flex-wrap gap-2">
          <Button type="button" size="compact-xs" variant="light" loading={requesting} onClick={() => void request()}>Request access</Button>
          <Button type="button" size="compact-xs" variant="default" onClick={() => void openSettings()}>Open System Settings</Button>
      </div>}
      {error && <div className="text-bad">{error}</div>}
    </div>
  );
}
