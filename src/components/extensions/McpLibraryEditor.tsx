import { Alert, Button, FocusTrap, Group, NativeSelect, SegmentedControl, Stack, Tabs, Textarea, TextInput } from "@mantine/core";
import { useState } from "react";
import { chooseFile } from "../../services/ipc/dialogs";
import { readTextFile } from "../../services/ipc/files";
import { mcpIpc } from "../../services/ipc/extensions";
import { toIpcError } from "../../services/ipc/common";
import type { AgentKind, McpAgent, McpChange, McpRow, McpServer } from "../../services/ipc/generated/core";
import { AgentTargets } from "./LibraryParts";
import { joinArgs, pairsText, parsePairs, splitArgs } from "./helpers";

const PRESETS = {
  github: { name: "github", transport: { type: "http", url: "https://api.githubcopilot.com/mcp/", headers: {}, sse: false }, enabled: true },
  context7: { name: "context7", transport: { type: "http", url: "https://mcp.context7.com/mcp", headers: {}, sse: false }, enabled: true },
  filesystem: { name: "filesystem", transport: { type: "stdio", command: "npx", args: ["-y", "@modelcontextprotocol/server-filesystem"], env: {} }, enabled: true },
} satisfies Record<string, McpServer>;

export function McpLibraryEditor({ initial, rows, agents, disabled, busy, onClose, onSave }: {
  initial: McpServer | null; rows: McpRow[]; agents: McpAgent[]; disabled: AgentKind[]; busy: boolean;
  onClose: () => void; onSave: (change: McpChange, onError: (message: string) => void) => Promise<boolean>;
}) {
  const row = rows.find((row) => row.name === initial?.name);
  const [tab, setTab] = useState<string | null>("form");
  const [name, setName] = useState(initial?.name ?? "");
  const [description, setDescription] = useState(row?.description ?? "");
  const initialTransport = initial?.transport;
  const [transport, setTransport] = useState(initialTransport?.type === "http" && initialTransport.sse ? "sse" : initialTransport?.type ?? "stdio");
  const [command, setCommand] = useState(initialTransport?.type === "stdio" ? initialTransport.command : "");
  const [args, setArgs] = useState(initialTransport?.type === "stdio" ? joinArgs(initialTransport.args) : "");
  const [env, setEnv] = useState(initialTransport?.type === "stdio" ? pairsText(initialTransport.env, "=") : "");
  const [url, setUrl] = useState(initialTransport?.type === "http" ? initialTransport.url : "");
  const [headers, setHeaders] = useState(initialTransport?.type === "http" ? pairsText(initialTransport.headers, ":") : "");
  const [targets, setTargets] = useState<AgentKind[]>(row?.bindings.filter((binding) => binding.wanted).map((binding) => binding.kind) ?? []);
  const [paste, setPaste] = useState("");
  const [parsed, setParsed] = useState<McpServer[] | null>(null);
  const [replace, setReplace] = useState(false);
  const [error, setError] = useState("");
  const [reading, setReading] = useState(false);
  const existing = new Set(rows.map((row) => row.name));
  const anySse = tab === "paste" ? parsed?.some((server) => server.transport.type === "http" && server.transport.sse) : transport === "sse";
  const blocked = anySse ? [...disabled, ...agents.filter((agent) => !agent.supportsSse).map((agent) => agent.kind)] : disabled;

  function preset(key: string) {
    const value: McpServer | undefined = PRESETS[key as keyof typeof PRESETS];
    if (!value) return;
    setName(value.name);
    setTransport(value.transport.type);
    if (value.transport.type === "stdio") {
      setCommand(value.transport.command);
      setArgs(joinArgs(value.transport.args));
      setEnv(pairsText(value.transport.env, "="));
    } else {
      setUrl(value.transport.url);
      setHeaders(pairsText(value.transport.headers, ":"));
    }
  }

  async function preview() {
    setReading(true);
    setError("");
    try { setParsed(await mcpIpc.parse(paste)); }
    catch (cause) { setError(toIpcError(cause).message); }
    finally { setReading(false); }
  }

  async function pickFile() {
    setError("");
    try {
      const path = await chooseFile();
      if (path) { setPaste(await readTextFile(path)); setParsed(null); }
    } catch (cause) { setError(toIpcError(cause).message); }
  }

  async function save() {
    setError("");
    const id = name.trim();
    if (tab === "form" && !initial && existing.has(id)) {
      setError("That MCP ID already exists. Open its row to edit it.");
      return;
    }
    const servers: McpServer[] = tab === "paste" ? parsed ?? [] : [{
      name: id, enabled: true, transport: transport === "stdio"
        ? { type: "stdio", command: command.trim(), args: splitArgs(args), env: parsePairs(env, "=") }
        : { type: "http", url: url.trim(), headers: parsePairs(headers, ":"), sse: transport === "sse" },
    }];
    const selected = targets.filter((target) => !blocked.includes(target));
    const saved = await onSave({ type: "save", servers, targets: selected, replace: Boolean(initial) || replace, description: description.trim() }, setError);
    if (saved) onClose();
  }

  return <>
    <FocusTrap.InitialFocus />
    <Stack gap="md">
      {!initial && <Tabs value={tab} onChange={(value) => { setTab(value); setError(""); }}>
        <Tabs.List><Tabs.Tab value="form">Server details</Tabs.Tab><Tabs.Tab value="paste">Paste JSON or TOML</Tabs.Tab></Tabs.List>
      </Tabs>}
      {tab === "form" ? <>
        {!initial && <NativeSelect label="Preset" defaultValue="" data={[
          { value: "", label: "Custom server" }, { value: "github", label: "GitHub" },
          { value: "context7", label: "Context7" }, { value: "filesystem", label: "Filesystem" },
        ]} onChange={(event) => preset(event.currentTarget.value)} />}
        <TextInput label="Server ID" placeholder="my-server" value={name} readOnly={Boolean(initial)} onChange={(event) => setName(event.currentTarget.value)} />
        <TextInput label="Description" value={description} maxLength={1000} onChange={(event) => setDescription(event.currentTarget.value)} />
        <SegmentedControl fullWidth aria-label="Transport" value={transport} onChange={setTransport} data={[
          { value: "stdio", label: "stdio" }, { value: "http", label: "Streamable HTTP" }, { value: "sse", label: "SSE" },
        ]} />
        {transport === "stdio" ? <>
          <TextInput label="Command" placeholder="npx" value={command} onChange={(event) => setCommand(event.currentTarget.value)} />
          <TextInput label="Arguments" value={args} onChange={(event) => setArgs(event.currentTarget.value)} />
          <Textarea label="Environment variables" value={env} description="One KEY=value per line. Masked values are kept." autosize minRows={2} maxRows={8} onChange={(event) => setEnv(event.currentTarget.value)} />
        </> : <>
          <TextInput label="Server URL" placeholder="https://example.com/mcp" value={url} onChange={(event) => setUrl(event.currentTarget.value)} />
          <Textarea label="Headers" value={headers} description="One Name: value per line. Masked values are kept." autosize minRows={2} maxRows={8} onChange={(event) => setHeaders(event.currentTarget.value)} />
        </>}
      </> : <>
        <Textarea label="MCP configuration" className="extension-config-input" description="Paste an mcpServers JSON object, OpenCode JSON or Codex TOML."
          value={paste} autosize minRows={8} maxRows={18} onChange={(event) => { setPaste(event.currentTarget.value); setParsed(null); }} />
        <Group><Button variant="default" onClick={() => void pickFile()}>Choose file</Button><Button variant="light" loading={reading} disabled={!paste.trim()} onClick={() => void preview()}>Preview servers</Button></Group>
        {parsed && <>
          <ul className="extension-import-preview">{parsed.map((server) => <li key={server.name}><strong>{server.name}</strong><span>{existing.has(server.name) ? "Existing" : "New"}</span></li>)}</ul>
          {parsed.some((server) => existing.has(server.name)) && <SegmentedControl aria-label="Existing servers" value={replace ? "replace" : "skip"}
            onChange={(value) => setReplace(value === "replace")} data={[{value: "skip", label: "Skip existing"}, {value: "replace", label: "Replace existing"}]} />}
        </>}
      </>}
      <AgentTargets targets={targets.filter((kind) => !blocked.includes(kind))} onChange={setTargets} disabled={blocked}
        hint={anySse ? "Select an agent that supports SSE." : "You can save the server with every agent off."} />
      {error && <Alert color="red" role="alert">{error}</Alert>}
      <Group justify="flex-end">
        <Button variant="default" disabled={busy} onClick={onClose}>Cancel</Button>
        <Button loading={busy} disabled={tab === "paste" ? !parsed?.length : !name.trim() || !(transport === "stdio" ? command.trim() : url.trim())}
          onClick={() => void save()}>{initial ? "Save changes" : tab === "paste" ? "Import servers" : "Add server"}</Button>
      </Group>
    </Stack>
  </>;
}
