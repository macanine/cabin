import { Button, Modal, SegmentedControl, TextInput } from "@mantine/core";
import { IconBrandApple, IconChevronRight, IconFolder, IconServer } from "@tabler/icons-react";
import { useEffect, useState } from "react";
import type { SyncMode, SyncPreview, SyncRules, Workspace } from "../services/ipc/generated/core";
import { libraryIpc } from "../services/ipc/library";
import { syncIpc } from "../services/ipc/sync";
import { chooseFolder } from "../services/ipc/dialogs";
import { IpcError, toIpcError } from "../services/ipc/common";
import { isFolderAccessError, macPermissionsIpc } from "../services/ipc/permissions";
import { FolderAccessHelp } from "./sync/FolderAccessHelp";
import { refreshSyncs } from "../state/store";
import { uiActions } from "../state/ui";
import { DEFAULT_SYNC_MODE, defaultGit, defaultRules, folderName, guestPathFor, modeInfo, previewSummary, slug, SYNC_MODES } from "../lib/syncForm";
import { SyncFiles } from "./sync/SyncFiles";
import { Reveal } from "./motion";
import { Note } from "./Boot";

type Source = "mac" | "machine";

/**
 * A new project: either a folder on this Mac kept in sync with a copy in the
 * machine (the usual case), or an empty folder that lives only in the
 * machine. For a Mac folder: its name, where it lands, which way changes
 * flow, and which files take part.
 */
export function NewWorkspaceModal({
  opened,
  onClose,
  onCreated,
}: {
  opened: boolean;
  onClose: () => void;
  /** Defaults to opening the new project. */
  onCreated?: (workspace: Workspace) => void;
}) {
  const [source, setSource] = useState<Source>("mac");
  const [hostPath, setHostPath] = useState<string | null>(null);
  const [name, setName] = useState("");
  const [guestPath, setGuestPath] = useState("");
  const [guestTouched, setGuestTouched] = useState(false);
  const [mode, setMode] = useState<SyncMode>(DEFAULT_SYNC_MODE);
  const [rules, setRules] = useState<SyncRules>(defaultRules(DEFAULT_SYNC_MODE));
  const [preview, setPreview] = useState<SyncPreview | null>(null);
  const [advanced, setAdvanced] = useState(false);
  const [error, setError] = useState<IpcError | null>(null);
  const [previewError, setPreviewError] = useState<IpcError | null>(null);
  const [retry, setRetry] = useState(0);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (!opened) return;
    setSource("mac"); setHostPath(null); setName(""); setGuestPath(""); setGuestTouched(false);
    setMode(DEFAULT_SYNC_MODE); setRules(defaultRules(DEFAULT_SYNC_MODE)); setPreview(null); setError(null); setPreviewError(null); setAdvanced(false);
  }, [opened]);

  const folder = guestTouched ? guestPath : guestPathFor(name);
  const ready = name.trim() && folder.trim() && (source === "machine" || (hostPath && preview));

  async function pick() {
    try {
      const picked = await chooseFolder(hostPath ?? undefined);
      if (!picked) return;
      setHostPath(picked);
      setPreview(null);
      setError(null);
      setRetry((value) => value + 1);
      if (!name.trim() || name === folderName(hostPath ?? "")) setName(folderName(picked));
    } catch (cause) {
      setError(toIpcError(cause));
    }
  }

  function chooseMode(next: SyncMode) {
    // Keep a deliberate git choice; otherwise follow the mode's safe default.
    setRules((current) => ({ ...current, git: current.git === defaultGit(mode) ? defaultGit(next) : current.git }));
    setMode(next);
  }

  async function create() {
    setBusy(true);
    setError(null);
    let workspace: Workspace | null = null;
    try {
      if (source === "mac" && hostPath) {
        const access = await macPermissionsIpc.checkFolder(hostPath, mode === "twoWay");
        if (access.state !== "available") {
          throw new IpcError({
            code: access.privacySettings ? "mac_folder_privacy" : "mac_folder_access",
            message: access.message ?? "The Mac folder is unavailable.",
          });
        }
      }
      workspace = await libraryIpc.createWorkspace(name.trim(), folder.trim());
      if (source === "mac" && hostPath) {
        await syncIpc.create({ workspaceId: workspace.id, name: name.trim(), hostPath, guestPath: workspace.path, mode, rules });
        await refreshSyncs();
      }
      if (onCreated) onCreated(workspace);
      else {
        onClose();
        uiActions.navigate({ kind: "new", workspaceId: workspace.id });
      }
    } catch (err) {
      // A project without its sync would look synced and not be: undo it.
      if (workspace && source === "mac") await libraryIpc.deleteWorkspace(workspace.id).catch(() => undefined);
      setError(toIpcError(err));
    } finally {
      setBusy(false);
    }
  }

  const mac = source === "mac";
  const problem = error ?? (mac ? previewError : null);
  const folderProblem = mac && hostPath && problem && isFolderAccessError(problem) ? problem : null;
  function retryFolder() {
    setError(null);
    setPreview(null);
    setRetry((value) => value + 1);
  }
  useEffect(() => {
    if (!opened || !folderProblem) return;
    window.addEventListener("focus", retryFolder);
    return () => window.removeEventListener("focus", retryFolder);
  }, [opened, folderProblem]);

  return (
    <Modal opened={opened} onClose={onClose} title="New project" size={480} centered>
      <form className="grid gap-3" onSubmit={(event) => { event.preventDefault(); if (ready) void create(); }}>
        <SegmentedControl
          fullWidth
          value={source}
          onChange={(value) => setSource(value as Source)}
          data={[
            { value: "mac", label: <span className="inline-flex items-center gap-1.5"><IconBrandApple size={14} />Mac folder</span> },
            { value: "machine", label: <span className="inline-flex items-center gap-1.5"><IconServer size={14} />Machine only</span> },
          ]}
        />
        <p className="m-0 text-xs leading-normal text-muted [&_code]:font-mono">
          {mac
            ? "Edit on your Mac, run agents in the machine. Both copies stay in sync."
            : <>An empty folder in the machine, for a fresh start or a <code>git clone</code>.</>}
        </p>

        {mac && (
          <button type="button" className={`press flex w-full cursor-pointer items-center gap-2 rounded-xl border px-3 py-2.5 text-left text-md hover:border-accent hover:bg-accent-soft ${hostPath ? "border-accent/45 bg-accent/7 text-fg" : "border-dashed border-line bg-surface/50 text-muted"}`} onClick={() => void pick()}>
            <IconFolder size={18} stroke={1.6} className="shrink-0 text-accent" />
            {hostPath
              ? <span className="min-w-0 flex-1 truncate font-mono text-xs" title={hostPath}>{hostPath}</span>
              : <span className="flex-1">Choose a folder…</span>}
            <span className="ml-auto text-xs font-medium text-accent">{hostPath ? "Change" : "Browse"}</span>
          </button>
        )}

        {(!mac || hostPath) && (
          <TextInput label="Name" placeholder="api-server" value={name} onChange={(e) => setName(e.currentTarget.value)} data-autofocus />
        )}

        {mac && hostPath && (
          <>
            <button type="button" className="flex cursor-pointer items-center gap-1.5 border-0 bg-transparent px-0 py-0.5 text-left text-md font-medium text-fg" aria-expanded={advanced} onClick={() => setAdvanced((v) => !v)}>
              <IconChevronRight size={13} className={`transition-transform duration-300 ease-out ${advanced ? "rotate-90" : ""}`} />
              <span>Sync options</span>
              <span className="ml-auto text-xs font-normal text-faint">{modeInfo(mode).label}{preview ? ` · ${previewSummary(preview)}` : ""}</span>
            </button>
            <Reveal open={advanced}>
              <div className="grid gap-3 pt-2">
                <div className="grid gap-1.5">
                  <span className="text-sm font-medium">Changes flow</span>
                  <SegmentedControl
                    fullWidth
                    size="xs"
                    value={mode}
                    onChange={(value) => chooseMode(value as SyncMode)}
                    data={SYNC_MODES.map((info) => ({ value: info.mode, label: info.label }))}
                  />
                  <span className="m-0 text-xs leading-normal text-muted [&_code]:font-mono">{modeInfo(mode).summary}</span>
                </div>
                <TextInput
                  label="Folder in the machine"
                  placeholder={guestPathFor(name)}
                  value={folder}
                  leftSection={<IconServer size={14} />}
                  onChange={(e) => { setGuestTouched(true); setGuestPath(e.currentTarget.value); }}
                />
                <SyncFiles hostPath={hostPath} mode={mode} rules={rules} onRules={setRules} onPreview={setPreview} onProblem={setPreviewError} retry={retry} />
              </div>
            </Reveal>
          </>
        )}

        {!mac && slug(name) && <p className="m-0 text-xs leading-normal text-muted [&_code]:font-mono">Created at <code>{folder}</code></p>}
        {hostPath && folderProblem ? (
          <FolderAccessHelp key={hostPath} path={hostPath} needsWrite={mode === "twoWay"} message={folderProblem.message}
            privacy={folderProblem.code === "mac_folder_privacy"} onCheck={retryFolder} />
        ) : problem && <Note tone="bad">{problem.message}</Note>}
        <footer className="mt-1 flex justify-end gap-2 border-t border-line/70 pt-3">
          <Button variant="default" onClick={onClose}>Cancel</Button>
          <Button type="submit" loading={busy} disabled={!ready}>{mac ? "Create and sync" : "Create project"}</Button>
        </footer>
      </form>
    </Modal>
  );
}
