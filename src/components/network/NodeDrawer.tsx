import { Button, Checkbox, Drawer, Loader, Menu, SegmentedControl, Select, Textarea, TextInput, Tooltip } from "@mantine/core";
import { IconBolt, IconChevronDown, IconDeviceLaptop, IconDownload, IconFileImport, IconFolderPlus, IconLink, IconPencil, IconPlus, IconSearch, IconTrash } from "@tabler/icons-react";
import { memo, useDeferredValue, useEffect, useMemo, useState } from "react";
import type { ProxyCatalog, ProxyGroup, ProxyNode, ProxyTestKind, ProxyTestResult } from "../../services/ipc/generated/core";
import { proxyIpc } from "../../services/ipc/network";
import { chooseFile, confirmAction } from "../../services/ipc/dialogs";
import { attempt } from "../../lib/operations";
import { relativeTime } from "../../lib/format";
import { hostTime } from "../../lib/clock";
import { formatGuestDateTime } from "../../lib/time";
import { flagInName } from "../../lib/geo";
import { bareName, NODE_DRAG } from "./RoutePipeline";
import { groupNodes, nodeKey, nodeMatches, type NodeGroup } from "./nodeCatalog";
import { queueNodeTests, testKey, type NodeTestState } from "./useNodeTests";
import { Flag } from "../Flag";

function testLabel(result: ProxyTestResult): string {
  if (!result.ok) return "Failed";
  if (result.kind === "latency") return `${result.latencyMs} ms`;
  const mbps = 0.008 * (result.bytes ?? 0) / (result.elapsedMs || 1);
  return `${Number(mbps.toPrecision(3))} Mbps`;
}

function TestButton({ node, kind, result, busy, bindInterface }: { node: ProxyNode; kind: ProxyTestKind; result?: ProxyTestResult; busy: boolean; bindInterface: string | null }) {
  const label = kind === "latency" ? "Test latency" : "Test download";
  const hint = kind === "latency" ? "Test latency" : "Test download speed with a 1 MiB sample";
  const value = result ? testLabel(result) : null;
  return (
    <button
      type="button"
      className="net-ms-button"
      disabled={busy}
      onClick={() => queueNodeTests([node], kind, bindInterface)}
      title={result ? `${hint} · ${result.detail} · ${formatGuestDateTime(result.testedAt)}` : hint}
      aria-label={`${label} for ${node.name}${value ? `, ${value}` : ""}${busy ? ", pending" : ""}`}
    >
      {busy ? <Loader size={10} /> : value ? <span className={`net-ms mono${!result?.ok ? " bad" : ""}`}>{value}</span>
        : kind === "latency" ? <IconBolt size={13} /> : <IconDownload size={13} />}
    </button>
  );
}

/** Add and remove sources, and recover a source when the route is closed. */
function Sources({ catalog, hostRefreshAllowed }: { catalog: ProxyCatalog; hostRefreshAllowed: boolean }) {
  const [mode, setMode] = useState<"url" | "paste" | "proxy">("url");
  const [name, setName] = useState("");
  const [value, setValue] = useState("");
  const [busy, setBusy] = useState<string | null>(null);

  async function run(key: string, task: Promise<unknown>, success: string) {
    setBusy(key);
    const result = await attempt(task, success);
    setBusy(null);
    return result;
  }

  async function add() {
    const task =
      mode === "proxy" ? proxyIpc.addUrl(value)
        : mode === "url" ? proxyIpc.importUrl(name, value)
          : proxyIpc.importText(name, value);
    if (await run("add", task, mode === "proxy" ? "Proxy added" : "Subscription imported")) {
      setName("");
      setValue("");
    }
  }

  return (
    <div className="net-sources">
      {catalog.sources.map((s) => (
        <div key={s.id} className="net-source">
          <div className="grow">
            <div className="truncate">{s.name}</div>
            <div className="faint fs-12">{s.nodeCount} {s.nodeCount === 1 ? "node" : "nodes"} · {relativeTime(hostTime(s.updatedAt))}</div>
          </div>
          {s.hasUrl && hostRefreshAllowed && (
            <Tooltip label="Recover subscription from this Mac">
              <button className="net-icon-button" type="button" disabled={busy != null} aria-label={`Recover ${s.name} from this Mac`}
                onClick={() => void run(`h-${s.id}`, proxyIpc.refreshFromHost(s.id), "Subscription recovered")}>
                {busy === `h-${s.id}` ? <Loader size={12} /> : <IconDeviceLaptop size={14} />}
              </button>
            </Tooltip>
          )}
          <Tooltip label="Remove">
            <button
              className="net-icon-button"
              disabled={busy != null}
              aria-label={`Remove ${s.name}`}
              onClick={async () => {
                if (await confirmAction(`Remove “${s.name}”?`)) void run(`d-${s.id}`, proxyIpc.remove(s.id), "Removed");
              }}
            >
              <IconTrash size={14} />
            </button>
          </Tooltip>
        </div>
      ))}
      <div className="net-source-add">
        <SegmentedControl
          size="xs"
          fullWidth
          value={mode}
          onChange={(v) => { setMode(v as typeof mode); setValue(""); }}
          data={[{ value: "url", label: "Subscription URL" }, { value: "paste", label: "Paste" }, { value: "proxy", label: "Proxy URL" }]}
        />
        {mode !== "proxy" && <TextInput size="xs" placeholder="Name" value={name} onChange={(e) => setName(e.currentTarget.value)} />}
        {mode === "paste" ? (
          <Textarea size="xs" autosize minRows={3} maxRows={8} placeholder="Clash YAML (plain or base64)" value={value} onChange={(e) => setValue(e.currentTarget.value)} className="mono" />
        ) : (
          <TextInput
            size="xs"
            leftSection={<IconLink size={13} />}
            placeholder={mode === "proxy" ? "socks5://host:port or http://user:pass@host:port" : "https://…"}
            value={value}
            onChange={(e) => setValue(e.currentTarget.value)}
          />
        )}
        <div className="row">
          {mode !== "proxy" && (
            <Button
              size="xs"
              variant="subtle"
              color="gray"
              leftSection={<IconFileImport size={13} />}
              disabled={!name.trim() || busy != null}
              onClick={async () => {
                const path = await chooseFile();
                if (path) await run("add", proxyIpc.importFile(name, path), "Subscription imported");
              }}
            >
              From file…
            </Button>
          )}
          <span className="grow" />
          <Button size="xs" loading={busy === "add"} disabled={!value.trim() || (mode !== "proxy" && !name.trim())} onClick={() => void add()}>
            Add
          </Button>
        </div>
      </div>
    </div>
  );
}

const PAGE = 100;

const NodeRow = memo(function NodeRow({ node, inRoute, latency, download, latencyBusy, downloadBusy, bindInterface, checked, onToggle, onAdd }: {
  node: ProxyNode;
  inRoute: boolean;
  latency?: ProxyTestResult;
  download?: ProxyTestResult;
  latencyBusy: boolean;
  downloadBusy: boolean;
  bindInterface: string | null;
  checked?: boolean;
  onToggle: (node: ProxyNode) => void;
  onAdd: (id: string) => void;
}) {
  const editing = checked !== undefined;
  const text = (
    <span className="grow net-node-text">
      <span className="truncate">{bareName(node.name)}</span>
      <span className="faint fs-12 truncate">{node.protocol} · {node.sourceName}</span>
    </span>
  );
  if (editing) {
    return (
      <label className="net-node net-node-member">
        <Checkbox size="xs" aria-label={`Include ${node.name} from ${node.sourceName}`} checked={checked} onChange={() => onToggle(node)} />
        {text}
      </label>
    );
  }
  return (
    <div
      className={`net-node${inRoute ? " in-route" : ""}`}
      draggable
      onDragStart={(event) => {
        event.dataTransfer.setData(NODE_DRAG, nodeKey(node));
        event.dataTransfer.effectAllowed = "copy";
      }}
    >
      <span className="net-flag-sm" aria-hidden>{flagInName(node.name) && <Flag code={flagInName(node.name)} size={11} />}</span>
      {text}
      <TestButton node={node} kind="latency" result={latency} busy={latencyBusy} bindInterface={bindInterface} />
      <TestButton node={node} kind="download" result={download} busy={downloadBusy} bindInterface={bindInterface} />
      <button
        type="button"
        className="net-icon-button"
        disabled={inRoute}
        onClick={() => onAdd(node.id)}
        title={inRoute ? "Already in the route" : "Add to the end of the route"}
        aria-label={`Add ${node.name} to the route`}
      >
        <IconPlus size={14} />
      </button>
    </div>
  );
});

/** Mount an initial page, then a page at a time as the drawer scrolls. */
function NodeList({ nodes, chain, tests, bindInterface, selected, onToggle, onAdd }: {
  nodes: ProxyNode[];
  chain: string[];
  tests: NodeTestState;
  bindInterface: string | null;
  selected: ReadonlySet<string> | undefined;
  onToggle: (node: ProxyNode) => void;
  onAdd: (id: string) => void;
}) {
  const [count, setCount] = useState(PAGE);
  const [sentinel, setSentinel] = useState<HTMLDivElement | null>(null);
  const inRoute = useMemo(() => new Set(chain), [chain]);
  useEffect(() => setCount(PAGE), [nodes]);
  useEffect(() => {
    if (!sentinel) return;
    const observer = new IntersectionObserver((entries) => {
      if (entries.some((entry) => entry.isIntersecting)) setCount((n) => n + PAGE);
    });
    observer.observe(sentinel);
    return () => observer.disconnect();
  }, [sentinel, count]);
  return (
    <div className="net-node-list">
      {nodes.slice(0, count).map((node) => {
        const latency = testKey(node, "latency", bindInterface);
        const download = testKey(node, "download", bindInterface);
        return (
          <NodeRow
            key={nodeKey(node)}
            node={node}
            inRoute={inRoute.has(node.id)}
            latency={tests.results.get(latency)}
            download={tests.results.get(download)}
            latencyBusy={tests.pending.has(latency)}
            downloadBusy={tests.pending.has(download)}
            bindInterface={bindInterface}
            checked={selected?.has(nodeKey(node))}
            onToggle={onToggle}
            onAdd={onAdd}
          />
        );
      })}
      {count < nodes.length && <div ref={setSentinel} className="faint fs-12 net-node-more">Showing {count} of {nodes.length}…</div>}
    </div>
  );
}

type DrawerProps = {
  catalog: ProxyCatalog;
  group: NodeGroup | null;
  onGroup: (group: NodeGroup | null) => void;
  chain: string[];
  tests: NodeTestState;
  bindInterface: string | null;
  hostRefreshAllowed: boolean;
  onAdd: (id: string) => void;
};

export function NodeDrawer({ opened, onClose, ...props }: DrawerProps & { opened: boolean; onClose: () => void }) {
  const [query, setQuery] = useState("");
  const [tab, setTab] = useState<"nodes" | "sources">("nodes");
  return (
    <Drawer opened={opened} onClose={onClose} position="right" size={410} title="Nodes" withOverlay={false} shadow="xl" className="net-drawer" lockScroll={false}>
      <DrawerBody {...props} query={query} onQuery={setQuery} tab={tab} onTab={setTab} />
    </Drawer>
  );
}

interface GroupEdit {
  id: string | null;
  name: string;
  selected: Set<string>;
}

function DrawerBody({ catalog, group, onGroup, chain, tests, bindInterface, hostRefreshAllowed, onAdd, query, onQuery: setQuery, tab, onTab: setTab }: DrawerProps & {
  query: string;
  onQuery: (query: string) => void;
  tab: "nodes" | "sources";
  onTab: (tab: "nodes" | "sources") => void;
}) {
  const [editor, setEditor] = useState<GroupEdit | null>(null);
  const [busy, setBusy] = useState(false);
  const [memberQuery, setMemberQuery] = useState("");
  const editing = editor !== null;
  const deferredQuery = useDeferredValue(editing ? memberQuery : query);
  const candidates = useMemo(() => groupNodes(catalog, editing ? null : group), [catalog, editing, group]);
  const visible = useMemo(() => candidates.filter((node) => nodeMatches(node, deferredQuery.trim().toLowerCase())), [candidates, deferredQuery]);
  const selected = catalog.groups.find((item) => `group:${item.id}` === group);
  const activeMembers = editor ? catalog.nodes.filter((node) => editor.selected.has(nodeKey(node))) : [];
  const latencyPending = visible.filter((node) => tests.pending.has(testKey(node, "latency", bindInterface))).length;
  const downloadPending = visible.filter((node) => tests.pending.has(testKey(node, "download", bindInterface))).length;

  function startEdit(group?: ProxyGroup) {
    const members = new Set(group?.nodeIds);
    setEditor({
      id: group?.id ?? null,
      name: group?.name ?? "",
      selected: new Set(catalog.nodes.filter((node) => members.has(node.id)).map(nodeKey)),
    });
    setMemberQuery("");
  }

  function toggleMember(node: ProxyNode) {
    setEditor((old) => {
      if (!old) return old;
      const key = nodeKey(node);
      const selected = new Set(old.selected);
      if (!selected.delete(key)) selected.add(key);
      return { ...old, selected };
    });
  }

  async function saveGroup() {
    if (!editor) return;
    setBusy(true);
    const result = await attempt(proxyIpc.saveGroup(editor.id, editor.name, activeMembers.map((node) => node.id)), "Group saved");
    setBusy(false);
    if (!result) return;
    const created = result.groups.find((item) => item.id === editor.id || !catalog.groups.some((group) => group.id === item.id));
    if (created) onGroup(`group:${created.id}`);
    setQuery("");
    setEditor(null);
  }

  async function removeGroup(item: ProxyGroup) {
    if (!await confirmAction(`Delete the group “${item.name}”? Its nodes stay in the catalog.`, "Delete group")) return;
    setBusy(true);
    const result = await attempt(proxyIpc.removeGroup(item.id), "Group deleted");
    setBusy(false);
    if (result) onGroup(null);
  }

  async function runBulk(kind: ProxyTestKind) {
    const needle = query.trim().toLowerCase();
    const nodes = candidates.filter((node) => nodeMatches(node, needle) && !tests.pending.has(testKey(node, kind, bindInterface)));
    if (kind === "download" && nodes.length > 1 && !await confirmAction(
      `Test download speeds for ${nodes.length} nodes? The tests use up to ${nodes.length} MiB in total.`, "Test downloads",
    )) return;
    queueNodeTests(nodes, kind, bindInterface);
  }

  return (
    <>
      <SegmentedControl
        size="xs"
        fullWidth
        value={tab}
        onChange={(value) => setTab(value as typeof tab)}
        data={[{ value: "nodes", label: "Nodes" }, { value: "sources", label: "Subscriptions" }]}
      />
      {tab === "sources" ? (
        <Sources catalog={catalog} hostRefreshAllowed={hostRefreshAllowed} />
      ) : (
        <>
          {editor ? (
            <form className="net-group-editor" onSubmit={(event) => { event.preventDefault(); void saveGroup(); }}>
              <TextInput size="xs" label="Group name" value={editor.name} maxLength={80} onChange={(event) => {
                const name = event.currentTarget.value;
                setEditor((old) => old && { ...old, name });
              }} />
              <div className="row">
                <span className="grow faint fs-12">{activeMembers.length} selected</span>
                <Button size="xs" variant="default" disabled={busy} onClick={() => setEditor(null)}>Cancel</Button>
                <Button type="submit" size="xs" loading={busy} disabled={!editor.name.trim()}>Save group</Button>
              </div>
            </form>
          ) : (
            <div className="net-drawer-tools">
              <Select
                className="grow"
                size="xs"
                aria-label="Filter by node group"
                searchable
                nothingFoundMessage="No group matches"
                data={[
                  { value: "all", label: "All nodes" },
                  { group: "Custom groups", items: catalog.groups.map((item) => ({ value: `group:${item.id}`, label: item.name })) },
                  { group: "Subscriptions", items: catalog.sources.map((item) => ({ value: `source:${item.id}`, label: item.name })) },
                ]}
                value={group ?? "all"}
                onChange={(value) => onGroup(value && value !== "all" ? value as NodeGroup : null)}
                allowDeselect={false}
              />
              <Menu position="bottom-end" shadow="md">
                <Menu.Target>
                  <Button size="xs" variant="default" rightSection={<IconChevronDown size={12} />} disabled={busy}>Groups</Button>
                </Menu.Target>
                <Menu.Dropdown>
                  <Menu.Item leftSection={<IconFolderPlus size={14} />} onClick={() => startEdit()}>New group</Menu.Item>
                  {selected && (
                    <>
                      <Menu.Item leftSection={<IconPencil size={14} />} onClick={() => startEdit(selected)}>Edit group</Menu.Item>
                      <Menu.Item color="red" leftSection={<IconTrash size={14} />} onClick={() => void removeGroup(selected)}>Delete group</Menu.Item>
                    </>
                  )}
                </Menu.Dropdown>
              </Menu>
            </div>
          )}
          <div className="net-drawer-tools">
            <TextInput
              size="xs"
              className="grow"
              leftSection={<IconSearch size={13} />}
              aria-label={editing ? "Find nodes to include in the group" : "Find a node"}
              placeholder="Find a node"
              value={editing ? memberQuery : query}
              onChange={(event) => (editing ? setMemberQuery : setQuery)(event.currentTarget.value)}
            />
            {!editing && (
              <Menu position="bottom-end" shadow="md">
                <Menu.Target>
                  <Button size="xs" variant="default" disabled={!visible.length} rightSection={<IconChevronDown size={12} />}>Test matches</Button>
                </Menu.Target>
                <Menu.Dropdown>
                  <Menu.Item leftSection={<IconBolt size={14} />} disabled={latencyPending === visible.length} onClick={() => void runBulk("latency")}>Latency</Menu.Item>
                  <Menu.Item leftSection={<IconDownload size={14} />} disabled={downloadPending === visible.length} onClick={() => void runBulk("download")}>Download speed · 1 MiB per node</Menu.Item>
                </Menu.Dropdown>
              </Menu>
            )}
          </div>
          {!editing && (latencyPending > 0 || downloadPending > 0) && (
            <div className="net-node-progress faint fs-12" role="status">
              {[latencyPending ? `${latencyPending} latency` : "", downloadPending ? `${downloadPending} download` : ""].filter(Boolean).join(" · ")} pending
            </div>
          )}
          {visible.length ? (
            <NodeList nodes={visible} chain={chain} tests={tests} bindInterface={bindInterface} selected={editor?.selected} onToggle={toggleMember} onAdd={onAdd} />
          ) : (
            <div className="net-drawer-empty">
              {catalog.nodes.length ? (selected && !editing && !query ? "This group has no nodes." : "No nodes match.") : "Add a subscription or proxy URL to see nodes here."}
              {catalog.nodes.length === 0 && <Button size="xs" variant="default" onClick={() => setTab("sources")}>Add a subscription</Button>}
            </div>
          )}
        </>
      )}
    </>
  );
}
