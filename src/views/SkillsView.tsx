import { ActionIcon, Alert, Badge, Button, Checkbox, Group, Menu, Modal, SegmentedControl, Table } from "@mantine/core";
import { IconDots } from "@tabler/icons-react";
import { useEffect, useRef, useState } from "react";
import { AGENT_KINDS } from "../components/AgentPicker";
import { AGENT_NAMES } from "../components/common";
import { Markdown } from "../components/chat/Markdown";
import { AgentColumn, AgentRail, BindingSwitch, ExtensionSearch, ExtensionStatus, useExtensionLibrary } from "../components/extensions/LibraryParts";
import { SkillDiscover } from "../components/extensions/SkillDiscover";
import { InstallSkillDialog, SkillBackupsDialog, SkillRepositoryDialog, type SkillInstallDialog } from "../components/extensions/SkillDialogs";
import { ViewHeader } from "../components/TitleBar";
import { attempt } from "../lib/operations";
import { confirmAction } from "../services/ipc/dialogs";
import { agentsIpc } from "../services/ipc/agents";
import { skillsIpc } from "../services/ipc/extensions";
import type { AgentKind, InstalledSkill, LocalSkill, SkillChange, SkillFile, SkillInstall, SkillMode } from "../services/ipc/generated/core";

export function SkillsView({ agent }: { agent?: AgentKind }) {
  const library = useExtensionLibrary(skillsIpc.list);
  const [tab, setTab] = useState("installed");
  const [query, setQuery] = useState("");
  const [filterAgent, setFilterAgent] = useState<AgentKind | null>(agent ?? null);
  const [sourceFilter, setSourceFilter] = useState("");
  const [updatesOnly, setUpdatesOnly] = useState(false);
  const [checking, setChecking] = useState(false);
  const [updates, setUpdates] = useState<string[] | null>(null);
  const [selected, setSelected] = useState<string[]>([]);
  const [dialog, setDialog] = useState<SkillInstallDialog | null>(null);
  const [manageRepos, setManageRepos] = useState(false);
  const [backups, setBackups] = useState(false);
  const [detail, setDetail] = useState<SkillFile | null>(null);
  const focusFolder = useRef<string | null>(null);
  const rowButton = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    if (tab !== "installed" || !focusFolder.current) return;
    rowButton.current?.focus();
    focusFolder.current = null;
  }, [tab]);
  const { data, busy } = library;
  const term = query.trim().toLowerCase();
  const matches = (...fields: string[]) => fields.some((value) => value.toLowerCase().includes(term));
  const sources = [...new Set(data?.installed.map(({ skill }) => skill.origin?.repository.repository ?? "Local / ZIP") ?? [])].sort();
  const installed = data?.installed.filter(({ skill, bindings }) => {
    const source = skill.origin?.repository.repository ?? "Local / ZIP";
    return (!filterAgent || bindings.some((binding) => binding.kind === filterAgent && binding.state !== "disabled"))
      && (!sourceFilter || source === sourceFilter) && (!updatesOnly || updates?.includes(skill.folder))
      && matches(skill.name, skill.folder, skill.description, source);
  }) ?? [];
  const imported = new Set(data?.installed.map(({ skill }) => skill.importedFrom).filter(Boolean));
  const local = updatesOnly || sourceFilter && sourceFilter !== "Local / ZIP" ? [] : data?.local.filter(({ entry, kind }) =>
    !imported.has(entry.path) && (!filterAgent || filterAgent === kind) && matches(entry.name, entry.description, entry.path)) ?? [];
  const marked = installed.filter(({ skill }) => selected.includes(skill.folder));

  function change(action: SkillChange) { return library.change(() => skillsIpc.change(action)); }
  function install(source: SkillInstall, targets: AgentKind[], mode?: SkillMode) { return library.change(() => skillsIpc.install(source, targets, mode)); }
  async function checkUpdates() {
    setChecking(true);
    try {
      const found = await attempt(skillsIpc.updates());
      setUpdates(found ?? null);
      if (!found) setUpdatesOnly(false);
    } finally { setChecking(false); }
  }
  async function update(folders: string[]) {
    for (const folder of folders) {
      if (!await install({ type: "update", folder }, [])) break;
      setUpdates((current) => current?.filter((name) => name !== folder) ?? null);
    }
  }
  async function remove(folders: string[]) {
    const label = folders.length === 1 ? folders[0] : `${folders.length} skills`;
    if (await confirmAction(`Uninstall ${label}? You can restore the managed copies from Skill backups.`, "Uninstall")) {
      if (await change({ type: "remove", folders })) setSelected((current) => current.filter((folder) => !folders.includes(folder)));
    }
  }
  function bind(rows: InstalledSkill[], kind: AgentKind, enabled: boolean) {
    const folders = rows.filter((row) => row.bindings.some((binding) => binding.kind === kind
      && binding.state === (enabled ? "disabled" : "enabled"))).map(({ skill }) => skill.folder);
    if (folders.length) void change({ type: "bind", folders, targets: [kind], enabled });
  }
  function manage(folder: string) {
    focusFolder.current = folder;
    setQuery(folder);
    setFilterAgent(null);
    setSourceFilter("");
    setUpdatesOnly(false);
    setSelected([]);
    setTab("installed");
  }
  async function open(folder: string | LocalSkill) {
    const result = await attempt(typeof folder === "string" ? skillsIpc.read(folder) : agentsIpc.readSkill(folder.kind, folder.entry.path));
    if (result) setDetail(result);
  }

  return <>
    <ViewHeader title="Skills">
      <SegmentedControl size="xs" value={tab} onChange={setTab} aria-label="Skills page" data={[
        { value: "installed", label: "Installed" }, { value: "discover", label: "Discover" },
      ]} />
      <Menu position="bottom-end"><Menu.Target><Button size="compact-sm" disabled={!data || busy}>Add Skill</Button></Menu.Target>
        <Menu.Dropdown>
          <Menu.Item onClick={() => setDialog({ type: "zip" })}>Install from ZIP</Menu.Item>
          <Menu.Item onClick={() => setDialog({ type: "local" })}>Import from agents</Menu.Item>
        </Menu.Dropdown>
      </Menu>
      <Menu position="bottom-end"><Menu.Target><ActionIcon variant="subtle" color="gray" aria-label="Skills actions"><IconDots size={17} /></ActionIcon></Menu.Target>
        <Menu.Dropdown>
          <Menu.Item disabled={busy || checking || !data?.installed.length} onClick={() => void checkUpdates()}>{checking ? "Checking for updates…" : "Check for updates"}</Menu.Item>
          <Menu.Item disabled={busy || !data?.installed.length} onClick={() => void change({ type: "sync" })}>Sync skills to agents</Menu.Item>
          <Menu.Item disabled={!data} onClick={() => setManageRepos(true)}>Manage repositories</Menu.Item>
          <Menu.Item disabled={!data} onClick={() => setBackups(true)}>Skill backups</Menu.Item>
        </Menu.Dropdown>
      </Menu>
    </ViewHeader>
    <div className="split-page">
    {data && tab === "installed" && <AgentRail label="Filter skills" value={filterAgent} onChange={setFilterAgent} total={data.installed.length}
      count={(kind) => data.installed.filter(({ bindings }) => bindings.some((binding) => binding.kind === kind && binding.state !== "disabled")).length}>
      {sources.length > 1 && <div className="split-label">Source</div>}
      {sources.length > 1 && sources.map((source) => (
        <button key={source} type="button" className="split-choice press" aria-pressed={sourceFilter === source} title={source} onClick={() => setSourceFilter(sourceFilter === source ? "" : source)}>
          <span>{source}</span>
        </button>
      ))}
      <div className="split-label">Maintenance</div>
      <button type="button" className="split-choice press" aria-pressed={updatesOnly} disabled={checking || !data.installed.length}
        onClick={() => updates == null ? void checkUpdates() : setUpdatesOnly(!updatesOnly)}>
        <span>{checking ? "Checking…" : updates == null ? "Check for updates" : "Updates"}</span>{updates != null && <span className="split-count">{updates.length}</span>}
      </button>
    </AgentRail>}
    <section className="extension-page" aria-label="Skills library">
      {!data ? <ExtensionStatus error={library.error} /> : <>
        {tab === "discover" ? <SkillDiscover data={data} busy={busy} onManage={manage}
          onInstall={(name, source) => setDialog({ type: "remote", name, source })} /> : <>
          <div className="extension-toolbar"><ExtensionSearch value={query} onChange={setQuery} placeholder="Search installed skills" />
          </div>
          {updates?.length === 0 && <Alert color="teal">Managed skills are up to date.</Alert>}
          {marked.length > 0 && <div className="extension-toolbar extension-bulk">
            <strong>{marked.length} selected</strong>
            <Menu><Menu.Target><Button variant="default" size="compact-sm" disabled={busy}>Agent settings</Button></Menu.Target>
              <Menu.Dropdown>{AGENT_KINDS.map((kind) => <Menu.Label key={kind}>{AGENT_NAMES[kind]}
                <Group gap="xs" wrap="nowrap">
                  <Button size="compact-xs" variant="subtle" disabled={!marked.some(({ bindings }) => bindings.some((binding) => binding.kind === kind && binding.state === "disabled"))} onClick={() => bind(marked, kind, true)}>Enable</Button>
                  <Button size="compact-xs" variant="subtle" disabled={!marked.some(({ bindings }) => bindings.some((binding) => binding.kind === kind && binding.state === "enabled"))} onClick={() => bind(marked, kind, false)}>Disable</Button>
                </Group>
              </Menu.Label>)}</Menu.Dropdown>
            </Menu>
            <Button variant="light" size="compact-sm" disabled={busy || !marked.some(({ skill }) => updates?.includes(skill.folder))}
              onClick={() => void update(marked.map(({ skill }) => skill.folder).filter((folder) => updates?.includes(folder)))}>Update selected</Button>
            <Button color="red" variant="light" size="compact-sm" disabled={busy} onClick={() => void remove(marked.map(({ skill }) => skill.folder))}>Uninstall</Button>
          </div>}
          <div className="extension-table-wrap"><Table className="extension-table" verticalSpacing="sm">
            <Table.Thead><Table.Tr><Table.Th className="extension-select-cell"><Checkbox aria-label="Select visible skills" checked={installed.length > 0 && marked.length === installed.length}
              indeterminate={marked.length > 0 && marked.length < installed.length} disabled={!installed.length || busy}
              onChange={(event) => setSelected(event.currentTarget.checked ? installed.map(({ skill }) => skill.folder) : [])} /></Table.Th>
              <Table.Th>Skill</Table.Th>{AGENT_KINDS.map((kind) => <Table.Th className="extension-matrix-cell" key={kind}><AgentColumn kind={kind} /></Table.Th>)}<Table.Th className="extension-row-actions" /></Table.Tr></Table.Thead>
            <Table.Tbody>
              {installed.map((row) => {
                const { skill, bindings } = row;
                return <Table.Tr key={skill.folder}>
                  <Table.Td className="extension-select-cell"><Checkbox aria-label={`Select ${skill.name}`} checked={selected.includes(skill.folder)} disabled={busy}
                    onChange={(event) => setSelected(event.currentTarget.checked ? [...selected, skill.folder] : selected.filter((folder) => folder !== skill.folder))} /></Table.Td>
                  <Table.Td><button type="button" ref={focusFolder.current === skill.folder ? rowButton : undefined}
                    className="extension-row-info extension-row-button" onClick={() => void open(skill.folder)}>
                    <Group gap="xs" wrap="nowrap"><strong>{skill.name}</strong>{updates?.includes(skill.folder) && <Badge color="blue" variant="light" size="xs">Update</Badge>}</Group>
                    <span title={skill.description}>{[skill.origin?.repository.repository, skill.description].filter(Boolean).join(" · ") || skill.folder}</span>
                  </button></Table.Td>
                  {bindings.map((binding) => <Table.Td className="extension-matrix-cell" key={binding.kind}><BindingSwitch {...binding} name={skill.name} disabled={busy}
                    onChange={(enabled) => void change({ type: "bind", folders: [skill.folder], targets: [binding.kind], enabled })} /></Table.Td>)}
                  <Table.Td className="extension-row-actions"><Menu position="bottom-end" withinPortal>
                    <Menu.Target><ActionIcon variant="subtle" color="gray" aria-label={`Actions for ${skill.name}`}><IconDots size={17} /></ActionIcon></Menu.Target>
                    <Menu.Dropdown><Menu.Item onClick={() => void open(skill.folder)}>Read SKILL.md</Menu.Item>
                      {skill.origin && <Menu.Item disabled={busy} onClick={() => void update([skill.folder])}>Update from repository</Menu.Item>}
                      <Menu.Item color="red" disabled={busy} onClick={() => void remove([skill.folder])}>Uninstall</Menu.Item>
                    </Menu.Dropdown>
                  </Menu></Table.Td>
                </Table.Tr>;
              })}
              {local.map((local) => <Table.Tr key={`${local.kind}:${local.entry.path}`}>
                <Table.Td className="extension-select-cell" /><Table.Td><button type="button" className="extension-row-info extension-row-button" onClick={() => void open(local)}>
                  <strong>{local.entry.name}</strong>
                  <span title={local.entry.path}>{local.entry.description || local.entry.path}</span>
                </button></Table.Td>
                {AGENT_KINDS.map((kind) => <Table.Td key={kind} className="extension-matrix-cell">{kind === local.kind && <span className="extension-unavailable">Local</span>}</Table.Td>)}
                <Table.Td className="extension-row-actions" />
              </Table.Tr>)}
            </Table.Tbody></Table>
            {!installed.length && !local.length && <div className="extension-empty">{term || updatesOnly || filterAgent || sourceFilter ? "No matching skills" : "Discover skills, install a ZIP or import from an agent."}</div>}
          </div>
        </>}
      </>}
    </section>
    </div>
    {dialog && data && <InstallSkillDialog dialog={dialog} local={data.local} initialAgent={filterAgent ?? undefined} busy={busy}
      onClose={() => setDialog(null)} onInstall={async (source, targets, mode) => {
        const saved = await install(source, targets, mode);
        if (saved) setTab("installed");
        return saved;
      }} />}
    {manageRepos && data && <SkillRepositoryDialog repositories={data.repositories} onClose={() => setManageRepos(false)} onSaved={() => void library.reload()} />}
    {backups && data && <SkillBackupsDialog backups={data.backups} busy={busy} onClose={() => setBackups(false)} onChange={change} />}
    {detail && <Modal opened title={detail.name} onClose={() => setDetail(null)} size="lg" centered>
      {detail.description && <p>{detail.description}</p>}<span className="extension-path">{detail.path}</span>
      <div className="extension-skill-markdown"><Markdown text={detail.body} /></div>
    </Modal>}
  </>;
}
