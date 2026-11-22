import { useAutoRefresh } from "../../lib/useAutoRefresh";
import { ActionIcon, Button, Loader, Modal, Select, TextInput, Tooltip } from "@mantine/core";
import { IconArchive, IconFolder, IconSearch } from "@tabler/icons-react";
import { useCallback, useEffect, useState, type KeyboardEvent } from "react";
import type { AgentKind, BackupEntry, SkillEntry, SkillFile } from "../../services/ipc/generated/core";
import { agentsIpc } from "../../services/ipc/agents";
import { toIpcError } from "../../services/ipc/common";
import { confirmAction } from "../../services/ipc/dialogs";
import { formatGuestDateTime } from "../../lib/time";
import { runOperation } from "../../lib/operations";
import { invalidateCatalogs } from "../../state/agentCatalog";
import { useApp } from "../../state/store";
import { CopyTargets } from "./CopyTargets";
import { SkillDetail } from "./SkillDetail";

export function AgentExtensionsPanel({ kind }: { kind: AgentKind }) {
  const workspaces = useApp((state) => state.workspaces.filter((workspace) => !workspace.archived));
  const [workspace, setWorkspace] = useState<string | null>(null);
  const workspaceId = workspaces.some((entry) => entry.id === workspace) ? workspace : workspaces[0]?.id ?? null;
  const [skills, setSkills] = useState<SkillEntry[]>([]);
  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState<string | null>(null);
  const [file, setFile] = useState<SkillFile | null>(null);
  const [fileError, setFileError] = useState("");
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(false);
  const [revision, setRevision] = useState(0);
  const [copying, setCopying] = useState(false);
  const [backups, setBackups] = useState<{ entries: BackupEntry[]; loading: boolean } | null>(null);
  const refresh = useCallback(() => setRevision((revision) => revision + 1), []);
  useAutoRefresh(refresh, { enabled: !!workspaceId, initial: false, interval: 30_000 });
  useEffect(() => { setSkills([]); setFile(null); }, [kind, workspaceId]);
  const term = query.trim().toLowerCase();
  const visible = skills.filter((entry) => [entry.name, entry.description, entry.path].some((value) => value.toLowerCase().includes(term)));
  const skill = visible.find((entry) => entry.path === selected) ?? visible[0] ?? null;
  const selectedPath = skill?.path;
  const detail = file?.path === selectedPath ? file : null;

  useEffect(() => {
    let current = true;
    setError("");
    if (!workspaceId) { setLoading(false); return; }
    setLoading(true);
    void agentsIpc.extensions(kind, workspaceId).then((extensions) => {
      if (current) setSkills(extensions.skills.filter((entry) => entry.scope === "project"));
    }, (cause: unknown) => {
      if (current) setError(toIpcError(cause).message);
    }).finally(() => { if (current) setLoading(false); });
    return () => { current = false; };
  }, [kind, workspaceId, revision]);

  useEffect(() => {
    let current = true;
    setFileError("");
    if (!selectedPath) return;
    void agentsIpc.readSkill(kind, selectedPath, workspaceId).then((value) => {
      if (current) setFile(value);
    }, (cause: unknown) => { if (current) setFileError(toIpcError(cause).message); });
    return () => { current = false; };
  }, [kind, workspaceId, selectedPath, revision]);

  async function copy(targets: AgentKind[]) {
    if (!skill) return;
    const record = await runOperation(agentsIpc.copySkill(kind, skill.path, targets, workspaceId), `${skill.name} copied`);
    if (record?.state === "succeeded") invalidateCatalogs();
  }

  async function remove(entry: SkillEntry) {
    if (!await confirmAction(`Delete the project skill “${entry.name}”? Its folder moves to the agent’s backups.`)) return;
    const record = await runOperation(agentsIpc.deleteSkill(kind, entry.path, workspaceId), `${entry.name} deleted`);
    if (record?.state === "succeeded") { invalidateCatalogs(kind); refresh(); }
  }

  async function openBackups() {
    setBackups({ loading: true, entries: [] });
    try {
      const entries = await agentsIpc.backups(kind);
      setBackups((current) => current && { loading: false, entries });
    } catch (cause) {
      setBackups(null);
      setError(toIpcError(cause).message);
    }
  }

  async function restore(path: string) {
    if (!await confirmAction("Restore this backup? The current file is backed up first.")) return;
    const record = await runOperation(agentsIpc.restoreBackup(kind, path), "Backup restored");
    if (record?.state === "succeeded") refresh();
  }

  function moveFocus(event: KeyboardEvent<HTMLDivElement>) {
    if (!["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) return;
    const rows = Array.from(event.currentTarget.querySelectorAll<HTMLElement>('[role="option"]'));
    const at = rows.indexOf(document.activeElement as HTMLElement);
    if (at < 0) return;
    event.preventDefault();
    const next = event.key === "Home" ? 0 : event.key === "End" ? rows.length - 1
      : Math.min(rows.length - 1, Math.max(0, at + (event.key === "ArrowDown" ? 1 : -1)));
    rows[next]?.focus();
  }

  return <div className="x-panel">
    <div className="x-toolbar">
      {workspaces.length > 0 && <Select size="xs" className="x-workspace" aria-label="Workspace for project skills"
        leftSection={<IconFolder size={13} />} value={workspaceId} onChange={setWorkspace}
        data={workspaces.map((entry) => ({ value: entry.id, label: entry.name }))} allowDeselect={false} />}
      <TextInput size="xs" className="x-search" placeholder="Search project skills" aria-label="Search project skills"
        leftSection={<IconSearch size={14} />} value={query} onChange={(event) => setQuery(event.currentTarget.value)} />
      <span className="grow" />
      <Tooltip label="Agent backups"><ActionIcon size="sm" variant="default" aria-label="Agent backups" onClick={() => void openBackups()}><IconArchive size={15} /></ActionIcon></Tooltip>
    </div>
    {error && <p className="x-error" role="alert">{error}</p>}
    <div className="x-split">
      <div className="x-list" role="listbox" aria-label="Project skills" onKeyDown={moveFocus}>
        {loading ? <div className="x-loading"><Loader size="xs" /> Reading files…</div>
          : !visible.length ? <p className="x-empty">{term ? "No skills match" : "No project skills"}</p>
          : visible.map((entry) => <button key={entry.path} type="button" role="option" aria-selected={entry.path === selectedPath}
            className={`x-row${entry.path === selectedPath ? " selected" : ""}`} onClick={() => setSelected(entry.path)}>
            <span className="x-row-main"><span className="x-row-title"><strong>{entry.name}</strong></span>
              <span className="x-row-sub truncate">{entry.description || entry.path}</span></span>
          </button>)}
      </div>
      {skill && <div className="x-detail-pane"><SkillDetail skill={skill} file={detail} loading={!detail && !fileError}
        error={fileError || null} onCopy={() => setCopying(true)} onDelete={() => void remove(skill)} /></div>}
    </div>
    <CopyTargets opened={copying} kind={kind} label={skill?.name ?? ""} onClose={() => setCopying(false)} onCopy={copy} />
    <Modal opened={backups != null} onClose={() => setBackups(null)} title="Agent backups" size="md">
      {backups?.loading ? <div className="x-loading"><Loader size="xs" /> Reading backups…</div>
        : !backups?.entries.length ? <p className="x-empty">No backups yet.</p>
        : <div className="x-backups">{backups.entries.map((entry) => <div key={entry.path} className="x-backup-row">
          <div><strong className="truncate" title={entry.path}>{entry.name}</strong><span>{formatGuestDateTime(entry.createdAt)}</span></div>
          <Button size="compact-xs" variant="default" onClick={() => void restore(entry.path)}>Restore</Button>
        </div>)}</div>}
    </Modal>
  </div>;
}
