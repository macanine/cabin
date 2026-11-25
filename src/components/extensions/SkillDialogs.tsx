import { Alert, Badge, Button, FileInput, Group, Modal, NativeSelect, Stack, TextInput } from "@mantine/core";
import { IconTrash } from "@tabler/icons-react";
import { useState } from "react";
import { formatGuestDateTime } from "../../lib/time";
import { toIpcError } from "../../services/ipc/common";
import { confirmAction } from "../../services/ipc/dialogs";
import { skillsIpc } from "../../services/ipc/extensions";
import type { AgentKind, LocalSkill, SkillBackup, SkillChange, SkillInstall, SkillMode, SkillRepository } from "../../services/ipc/generated/core";
import { AgentLogo, AGENT_NAMES } from "../common";
import { AgentTargets, ExtensionSearch } from "./LibraryParts";

export type SkillInstallDialog =
  | { type: "zip" }
  | { type: "local" }
  | { type: "remote"; name: string; source: Extract<SkillInstall, { type: "repository" | "registry" }> };

const readZip = (file: File): Promise<string> => new Promise((resolve, reject) => {
  const reader = new FileReader();
  reader.onload = () => {
    const base64 = typeof reader.result === "string" ? reader.result.split(",", 2)[1] : undefined;
    if (base64) resolve(base64); else reject(new Error("The ZIP is empty"));
  };
  reader.onerror = () => reject(reader.error ?? new Error("Could not read the skill ZIP"));
  reader.readAsDataURL(file);
});

export function InstallSkillDialog({ dialog, local, busy, initialAgent, onClose, onInstall }: {
  dialog: SkillInstallDialog; local: LocalSkill[]; busy: boolean; initialAgent?: AgentKind;
  onClose: () => void; onInstall: (source: SkillInstall, targets: AgentKind[], mode: SkillMode) => Promise<boolean>;
}) {
  const [targets, setTargets] = useState<AgentKind[]>(initialAgent ? [initialAgent] : []);
  const [mode, setMode] = useState<SkillMode>("link");
  const [zip, setZip] = useState<File | null>(null);
  const [selected, setSelected] = useState<LocalSkill | null>(null);
  const [query, setQuery] = useState("");
  const [error, setError] = useState("");
  const [reading, setReading] = useState(false);
  const locked = reading || busy;
  const term = query.trim().toLowerCase();
  const visible = local.filter(({ entry, kind }) => [entry.name, entry.path, AGENT_NAMES[kind]].some((field) => field.toLowerCase().includes(term)));

  async function submit() {
    if (locked) return;
    setError("");
    setReading(true);
    try {
      let source: SkillInstall;
      if (dialog.type === "remote") source = dialog.source;
      else if (dialog.type === "local" && selected) source = { type: "local", kind: selected.kind, path: selected.entry.path };
      else if (dialog.type === "zip" && zip) {
        if (zip.size > 12 * 1024 * 1024) throw new Error("Choose a ZIP up to 12 MiB");
        source = { type: "zip", contents: await readZip(zip) };
      } else return;
      const enabled = targets.filter((kind) => kind !== selected?.kind);
      if (await onInstall(source, enabled, mode)) onClose();
    } catch (cause) { setError(toIpcError(cause).message); }
    finally { setReading(false); }
  }

  return <Modal opened onClose={onClose} withCloseButton={!locked} closeOnClickOutside={!locked} closeOnEscape={!locked} centered size="lg"
    title={dialog.type === "remote" ? `Install ${dialog.name}` : dialog.type === "zip" ? "Install skill from ZIP" : "Import an agent’s skill"}>
    <Stack gap="md">
      {dialog.type === "remote" && <div className="text-sm text-muted break-words">{dialog.source.type === "repository"
        ? `${dialog.source.origin.repository.repository} / ${dialog.source.origin.path}` : dialog.source.repository}</div>}
      {dialog.type === "zip" && <FileInput label="Skill ZIP" accept=".zip,application/zip" placeholder="Choose a ZIP" value={zip} onChange={setZip} disabled={locked} clearable />}
      {dialog.type === "local" && <>
        <ExtensionSearch value={query} onChange={setQuery} placeholder="Find an agent’s skill" />
        <div className="extension-picker">
          {visible.map((skill) => <button type="button" className="extension-option" aria-pressed={selected === skill} key={`${skill.kind}:${skill.entry.path}`}
            disabled={locked} onClick={() => setSelected(skill)}>
            <AgentLogo agent={skill.kind} size={18} /><div className="extension-row-info"><strong>{skill.entry.name}</strong><span>{skill.entry.path}</span></div>
          </button>)}
          {!visible.length && <div className="extension-empty">No matching user skills</div>}
        </div>
      </>}
      <AgentTargets targets={targets.filter((kind) => kind !== selected?.kind)} onChange={setTargets} disabled={selected ? [selected.kind] : []}
        hint={selected ? `The original stays in ${AGENT_NAMES[selected.kind]}.` : "Leave every agent off to install into the library only."} />
      <NativeSelect label="Agent folders" value={mode} disabled={locked} data={[
        { value: "link", label: "Link to the managed skill" }, { value: "copy", label: "Keep a copy in each agent folder" },
      ]} onChange={(event) => setMode(event.currentTarget.value === "copy" ? "copy" : "link")} />
      {error && <Alert color="red" role="alert">{error}</Alert>}
      <Group justify="flex-end"><Button variant="default" disabled={locked} onClick={onClose}>Cancel</Button>
        <Button loading={locked} disabled={dialog.type === "zip" && !zip || dialog.type === "local" && !selected} onClick={() => void submit()}>
          {dialog.type === "local" ? "Import skill" : "Install skill"}
        </Button>
      </Group>
    </Stack>
  </Modal>;
}

export function SkillRepositoryDialog({ repositories, onClose, onSaved }: {
  repositories: SkillRepository[]; onClose: () => void; onSaved: () => void;
}) {
  const [sources, setSources] = useState(repositories);
  const [repository, setRepository] = useState("");
  const [branch, setBranch] = useState("");
  const [directory, setDirectory] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  async function save(next: SkillRepository[]) {
    setError("");
    setBusy(true);
    try {
      setSources(await skillsIpc.repositories(next));
      onSaved();
      return true;
    } catch (cause) { setError(toIpcError(cause).message); return false; }
    finally { setBusy(false); }
  }

  return <Modal opened onClose={onClose} closeOnClickOutside={!busy} closeOnEscape={!busy} title="Skill repositories" centered size="lg">
    <Stack gap="md">
      <div className="extension-discovery-list">
        {sources.map((source, index) => <div className="extension-discovery-row" key={index}>
          <div className="extension-row-info"><strong>{source.repository}</strong><span>{source.branch}{source.directory && ` · ${source.directory}`}</span></div>
          <Button variant="subtle" color="gray" size="compact-sm" disabled={busy} onClick={() => void save(sources.filter((_, at) => at !== index))}>Remove</Button>
        </div>)}
        {!sources.length && <div className="extension-empty">No repositories configured</div>}
      </div>
      <form className="extension-repo-form" onSubmit={async (event) => {
        event.preventDefault();
        if (!repository.trim() || busy) return;
        if (await save([...sources, { repository, branch, directory }])) { setRepository(""); setBranch(""); setDirectory(""); }
      }}>
        <TextInput label="GitHub repository" placeholder="owner/repository or GitHub URL" value={repository} onChange={(event) => setRepository(event.currentTarget.value)} />
        <TextInput label="Branch or tag" placeholder="Default branch" value={branch} onChange={(event) => setBranch(event.currentTarget.value)} />
        <TextInput label="Skills subdirectory" placeholder="Optional" value={directory} onChange={(event) => setDirectory(event.currentTarget.value)} />
        {error && <Alert color="red" role="alert">{error}</Alert>}
        <Group justify="flex-end"><Button variant="default" onClick={onClose}>Done</Button><Button type="submit" disabled={!repository.trim()} loading={busy}>Add repository</Button></Group>
      </form>
    </Stack>
  </Modal>;
}

export function SkillBackupsDialog({ backups, busy, onClose, onChange }: {
  backups: SkillBackup[]; busy: boolean; onClose: () => void; onChange: (change: SkillChange) => Promise<boolean>;
}) {
  async function restore(backup: SkillBackup) {
    if (await confirmAction(`Restore ${backup.name} with its saved agent settings? An installed version will move to backups.`, "Restore skill")) {
      await onChange({ type: "restore", id: backup.id });
    }
  }
  async function remove(backup: SkillBackup) {
    if (await confirmAction(`Permanently delete this backup of ${backup.name}?`, "Delete backup")) {
      await onChange({ type: "deleteBackup", id: backup.id });
    }
  }
  return <Modal opened onClose={onClose} title="Skill backups" centered size="lg">
    <div className="extension-discovery-list">{backups.map((backup) => <div className="extension-discovery-row" key={backup.id}>
      <div className="extension-row-info"><strong>{backup.name}</strong><span>{formatGuestDateTime(Number(backup.at))}</span></div>
      <Group gap="xs"><Button size="compact-sm" variant="light" disabled={busy} onClick={() => void restore(backup)}>Restore</Button>
        <Button size="compact-sm" variant="subtle" color="red" aria-label={`Delete ${backup.name} backup`} disabled={busy} onClick={() => void remove(backup)}><IconTrash size={15} /></Button></Group>
    </div>)}</div>
    {!backups.length && <Badge color="gray" variant="light">No skill backups</Badge>}
  </Modal>;
}
