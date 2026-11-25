import { ActionIcon, Alert, Badge, Button, Drawer, Group, Menu, Table, Tooltip } from "@mantine/core";
import { IconCopy, IconDots, IconDownload } from "@tabler/icons-react";
import { useState } from "react";
import { AGENT_KINDS } from "../components/AgentPicker";
import { AGENT_NAMES } from "../components/common";
import { AgentColumn, AgentRail, BindingSwitch, ExtensionSearch, ExtensionStatus, useExtensionLibrary } from "../components/extensions/LibraryParts";
import { McpLibraryEditor } from "../components/extensions/McpLibraryEditor";
import { ViewHeader } from "../components/TitleBar";
import { attempt } from "../lib/operations";
import { clipboard, confirmAction } from "../services/ipc/dialogs";
import { mcpIpc } from "../services/ipc/extensions";
import type { AgentKind, McpChange, McpRow, McpServer } from "../services/ipc/generated/core";
import { useApp } from "../state/store";

function available(row: McpRow, kind: AgentKind, blocked: AgentKind[]) {
  const binding = row.bindings.find((binding) => binding.kind === kind);
  return Boolean(row.managed && !row.conflict && !blocked.includes(kind)
    && (binding?.state === "enabled" || binding?.state === "disabled"));
}

export function McpView({ agent: initialAgent }: { agent?: AgentKind }) {
  const [agent, setAgent] = useState<AgentKind | null>(initialAgent ?? null);
  const library = useExtensionLibrary(mcpIpc.list);
  const [query, setQuery] = useState("");
  const [writeError, setWriteError] = useState<{ title: string; message: string } | null>(null);
  const [editor, setEditor] = useState<{ server: McpServer | null; opened: boolean; revision: number } | null>(null);
  const [undo, setUndo] = useState<{ kind: AgentKind; names: string[]; enabled: boolean } | null>(null);
  const pi = useApp((state) => state.agents.find((entry) => entry.kind === "pi"));
  const legacyPi = pi?.installed && (pi.version?.match(/\b\d+\./)?.[0] === "0." || !pi.version && pi.package === "@mariozechner/pi-coding-agent");
  const { data, busy } = library;
  const blocked = data?.agents.filter((agent) => !agent.path || agent.error || agent.kind === "pi" && legacyPi).map((agent) => agent.kind) ?? [];
  const term = query.trim().toLowerCase();
  const visible = data?.rows.filter((row) => (!agent || row.bindings.some((binding) => binding.kind === agent && (binding.state === "enabled" || binding.wanted)))
    && [row.name, row.endpoint, row.description].some((value) => value.toLowerCase().includes(term))) ?? [];
  const importable = data?.rows.some((row) => !row.managed && !row.conflict && row.transport);

  function change(action: McpChange, onError?: (message: string) => void) {
    setUndo(null);
    setWriteError(null);
    return library.change(() => mcpIpc.change(action), onError ?? ((message) => {
      const subject = action.type === "bind" ? action.targets.map((kind) => AGENT_NAMES[kind]).join(", ") : "MCP configuration";
      const names = action.type === "bind" && action.names.length === 1 ? ` for ${action.names[0]}` : "";
      setWriteError({ title: `Could not update ${subject}${names}`, message });
    }));
  }
  async function column(kind: AgentKind, enabled: boolean) {
    const names = visible.filter((row) => available(row, kind, blocked) && row.bindings.some((binding) => binding.kind === kind
      && (binding.state === "enabled") !== enabled)).map((row) => row.name);
    if (names.length && await change({ type: "bind", names, targets: [kind], enabled })) setUndo({ kind, names, enabled: !enabled });
  }
  function openEditor(server: McpServer | null) {
    setEditor((current) => ({ server, opened: true, revision: (current?.revision ?? 0) + 1 }));
  }
  function closeEditor() {
    setEditor((current) => current?.opened ? { ...current, opened: false } : current);
  }
  async function edit(row: McpRow) {
    const server = await attempt(mcpIpc.edit(row.name));
    if (server) openEditor(server);
  }
  async function copy(name?: string) {
    const ok = await confirmAction("Copy server definitions to the clipboard? This can include secrets, but does not include which agents have the servers enabled.", "Copy JSON");
    if (ok) await attempt(mcpIpc.export(name).then(clipboard.write), "MCP configuration copied");
  }
  async function remove(row: McpRow) {
    if (await confirmAction(`Delete ${row.name} from the MCP library and its agent configurations?`, "Delete server")) {
      await change({ type: "remove", names: [row.name] });
    }
  }

  return <>
    <ViewHeader title="MCP" subtitle="User-level configuration for agents">
      <Button size="compact-sm" onClick={() => openEditor(null)} disabled={busy || !data}>Add MCP</Button>
      <Menu position="bottom-end"><Menu.Target><ActionIcon variant="subtle" color="gray" aria-label="MCP actions"><IconDots size={17} /></ActionIcon></Menu.Target>
        <Menu.Dropdown>
          <Menu.Item leftSection={<IconDownload size={16} />} disabled={busy || !importable} onClick={() => void change({ type: "import" })}>Import from agents</Menu.Item>
          <Menu.Item disabled={busy || !data?.rows.some((row) => row.managed)} onClick={() => void change({ type: "sync" })}>Sync managed servers to agents</Menu.Item>
          <Menu.Item leftSection={<IconCopy size={16} />} disabled={busy || !data?.rows.some((row) => row.managed)} onClick={() => void copy()}>Copy all as JSON</Menu.Item>
        </Menu.Dropdown>
      </Menu>
    </ViewHeader>
    <div className="split-page">
    {data && <AgentRail label="Filter servers" value={agent} onChange={setAgent} total={data.rows.length}
      count={(kind) => data.rows.filter((row) => row.bindings.some((binding) => binding.kind === kind && (binding.state === "enabled" || binding.wanted))).length} />}
    <section className="extension-page" aria-label="MCP servers">
      {!data ? <ExtensionStatus error={library.error} /> : <>
        <div className="extension-toolbar">
          <ExtensionSearch value={query} onChange={setQuery} placeholder="Search servers, commands or URLs" />
        </div>
        {data.agents.filter((agent) => agent.error).map((agent) => <Alert color="yellow" key={agent.kind} title={`${AGENT_NAMES[agent.kind]} MCP config could not be read`}>
          {agent.path && <span className="extension-path">{agent.path}</span>} {agent.error}
        </Alert>)}
        {legacyPi && <Alert color="gray">Native MCP requires Pi 1.0 or newer.</Alert>}
        {writeError && <Alert color="red" role="alert" title={writeError.title}
          withCloseButton closeButtonLabel="Dismiss configuration error" onClose={() => setWriteError(null)}>
          {writeError.message}
        </Alert>}
        {undo && <Alert color="blue"><Group justify="space-between">Updated {undo.names.length} servers for {AGENT_NAMES[undo.kind]}
          <Button variant="subtle" size="compact-sm" onClick={() => void change({ type: "bind", names: undo.names, targets: [undo.kind], enabled: undo.enabled })}>Undo</Button>
        </Group></Alert>}
        <div className="extension-table-wrap">
          <Table className="extension-table" verticalSpacing="sm">
            <Table.Thead><Table.Tr><Table.Th>Server</Table.Th>{AGENT_KINDS.map((kind) => <Table.Th className="extension-matrix-cell" key={kind}>
              <Menu withinPortal><Menu.Target><ActionIcon variant="subtle" color="gray" aria-label={`${AGENT_NAMES[kind]} bulk actions`}><AgentColumn kind={kind} /></ActionIcon></Menu.Target>
                <Menu.Dropdown>
                  <Menu.Item disabled={busy || !visible.some((row) => available(row, kind, blocked) && row.bindings.some((binding) => binding.kind === kind && binding.state === "disabled"))}
                    onClick={() => void column(kind, true)}>Enable visible servers</Menu.Item>
                  <Menu.Item disabled={busy || !visible.some((row) => available(row, kind, blocked) && row.bindings.some((binding) => binding.kind === kind && binding.state === "enabled"))}
                    onClick={() => void column(kind, false)}>Disable visible servers</Menu.Item>
                </Menu.Dropdown>
              </Menu>
            </Table.Th>)}<Table.Th className="extension-row-actions" /></Table.Tr></Table.Thead>
            <Table.Tbody>{visible.map((row) => <Table.Tr key={row.name}>
              <Table.Td><div className="extension-row-info"><Group gap="xs" wrap="nowrap"><strong>{row.name}</strong>
                {row.conflict ? <Badge size="xs" color="yellow" variant="light">Conflict</Badge>
                  : !row.transport ? <Badge size="xs" color="yellow" variant="light">Unreadable</Badge>
                  : !row.managed ? <Badge size="xs" color="gray" variant="light">From agents</Badge> : null}</Group>
                <span title={row.description || row.endpoint}>{row.description || [row.transport, row.endpoint].filter(Boolean).join(" · ")}</span>
              </div></Table.Td>
              {row.bindings.map((binding) => <Table.Td className="extension-matrix-cell" key={binding.kind}><BindingSwitch {...binding} name={row.name}
                note={binding.kind === "pi" && legacyPi ? "Requires Pi 1.0 or newer" : undefined}
                disabled={busy || !available(row, binding.kind, blocked)}
                onChange={(enabled) => void change({ type: "bind", names: [row.name], targets: [binding.kind], enabled })} /></Table.Td>)}
              <Table.Td className="extension-row-actions">{row.managed ? <Menu position="bottom-end" withinPortal>
                <Menu.Target><ActionIcon variant="subtle" color="gray" aria-label={`Actions for ${row.name}`}><IconDots size={17} /></ActionIcon></Menu.Target>
                <Menu.Dropdown>
                  <Menu.Item disabled={busy || row.conflict} onClick={() => void edit(row)}>Edit server</Menu.Item>
                  <Menu.Item disabled={busy} onClick={() => void copy(row.name)}>Copy as JSON</Menu.Item>
                  <Menu.Item disabled={busy || !AGENT_KINDS.some((kind) => available(row, kind, blocked))} onClick={() => void change({ type: "bind", names: [row.name], targets: AGENT_KINDS.filter((kind) => available(row, kind, blocked)), enabled: true })}>Enable on supported agents</Menu.Item>
                  <Menu.Item disabled={busy || !AGENT_KINDS.some((kind) => available(row, kind, blocked))} onClick={() => void change({ type: "bind", names: [row.name], targets: AGENT_KINDS.filter((kind) => available(row, kind, blocked)), enabled: false })}>Disable on supported agents</Menu.Item>
                  <Menu.Divider /><Menu.Item color="red" disabled={busy || row.conflict} onClick={() => void remove(row)}>Delete server</Menu.Item>
                </Menu.Dropdown>
              </Menu> : row.conflict && <Tooltip label="The agents have different configurations for this ID"><span aria-label="Conflicting native configurations">!</span></Tooltip>}</Table.Td>
            </Table.Tr>)}</Table.Tbody>
          </Table>
          {!visible.length && <div className="extension-empty">{term || agent ? "No matching MCP servers" : "Add an MCP server or import an existing configuration."}</div>}
        </div>
      </>}
    </section>
    </div>
    <Drawer opened={editor?.opened ?? false} onClose={closeEditor}
      onExitTransitionEnd={() => setEditor((current) => current?.opened ? current : null)}
      title={editor?.server ? `Edit ${editor.server.name}` : "Add MCP server"}
      position="right" size="lg" classNames={{ header: "extension-editor-header" }}
      closeOnClickOutside={!busy} closeOnEscape={!busy} withCloseButton={!busy}>
      {editor && data && <McpLibraryEditor key={editor.revision} initial={editor.server} rows={data.rows} agents={data.agents}
        disabled={blocked} busy={busy} onSave={change} onClose={closeEditor} />}
    </Drawer>
  </>;
}
