import { Button, Drawer, Loader, SegmentedControl, Textarea, TextInput, Tooltip } from "@mantine/core";
import { IconArrowLeft, IconBolt, IconCheck, IconDeviceLaptop, IconFileImport, IconLink, IconPlus, IconSearch, IconTrash } from "@tabler/icons-react";
import { memo, useDeferredValue, useMemo, useState } from "react";
import type { ProxyCatalog, ProxyNode } from "../../services/ipc/generated/core";
import { proxyIpc } from "../../services/ipc/network";
import { chooseFile, confirmAction } from "../../services/ipc/dialogs";
import { attempt } from "../../lib/operations";
import { relativeTime } from "../../lib/format";
import { hostTime } from "../../lib/clock";
import { formatGuestDateTime } from "../../lib/time";
import { flagInName } from "../../lib/geo";
import { bareName, NODE_DRAG } from "./RoutePipeline";
import { nodeKey, nodeMatches } from "./nodeCatalog";
import { queueNodeTests, useCanTestNodes, useNodeTest } from "./useNodeTests";
import { Flag } from "../Flag";

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
            <Tooltip label="Recover subscription from this computer">
              <button className="net-icon-button" type="button" disabled={busy != null} aria-label={`Recover ${s.name} from this computer`}
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

const PAGE = 48;

const NodeCard = memo(function NodeCard({ node, inRoute, onAdd }: {
  node: ProxyNode;
  inRoute: boolean;
  onAdd: (id: string) => void;
}) {
  const { result, pending } = useNodeTest(node);
  const country = flagInName(node.name);
  const name = bareName(node.name);
  const latency = result?.latencyMs;
  const value = result ? result.ok && latency != null ? `${latency} ms` : "Failed" : "—";
  const tone = !result ? "text-muted" : !result.ok ? "text-bad" : (latency ?? 0) > 350 ? "text-warn" : "text-ok";
  return (
    <div
      className={`flex min-w-0 cursor-grab items-center gap-1.5 rounded-lg border px-2 py-1.5 ${inRoute ? "border-accent/50 bg-accent/5" : "border-line bg-surface"}`}
      draggable
      onDragStart={(event) => {
        event.dataTransfer.setData(NODE_DRAG, nodeKey(node));
        event.dataTransfer.effectAllowed = "copy";
      }}
    >
      {country && <span className="inline-flex shrink-0" aria-hidden><Flag code={country} size={13} /></span>}
      <span className="min-w-0 flex-1 truncate text-sm" title={name}>{name}</span>
      <button
        type="button"
        className={`inline-flex h-6 min-w-11 shrink-0 items-center justify-end whitespace-nowrap rounded-md border-0 bg-transparent px-1 text-xs tabular-nums enabled:cursor-pointer enabled:hover:bg-hover ${tone}`}
        disabled={pending}
        onClick={() => queueNodeTests([node])}
        title={result ? `${result.detail} · ${formatGuestDateTime(result.testedAt)}` : "Test latency"}
        aria-label={`Test latency for ${node.name}${result ? `, ${value}` : ""}${pending ? ", pending" : ""}`}
      >
        {pending ? <Loader size={11} /> : value}
      </button>
      <button
        type="button"
        className={`net-icon-button${inRoute ? " text-accent" : ""}`}
        disabled={inRoute}
        onClick={() => onAdd(node.id)}
        title={inRoute ? "Already in the route" : "Add to route"}
        aria-label={inRoute ? `${node.name} is in the route` : `Add ${node.name} to the route`}
      >
        {inRoute ? <IconCheck size={13} /> : <IconPlus size={13} />}
      </button>
    </div>
  );
});

type DrawerProps = {
  opened: boolean;
  onClose: () => void;
  catalog: ProxyCatalog;
  chain: string[];
  hostRefreshAllowed: boolean;
  onAdd: (id: string) => void;
};

export function NodeDrawer({ opened, onClose, catalog, chain, hostRefreshAllowed, onAdd }: DrawerProps) {
  const [sources, setSources] = useState(false);
  const [searching, setSearching] = useState(false);
  const [query, setQuery] = useState("");
  const [count, setCount] = useState(PAGE);
  const needle = useDeferredValue(query.trim().toLowerCase());
  const inRoute = useMemo(() => new Set(chain), [chain]);
  const visible = useMemo(() => catalog.nodes.filter((node) => nodeMatches(node, needle)), [catalog.nodes, needle]);
  const testable = useCanTestNodes(visible);
  const testLabel = needle ? "Test matching nodes" : "Test all nodes";

  return (
    <Drawer.Root opened={opened} onClose={onClose} position="right" size="min(640px, 100vw)" shadow="xl" className="net-drawer" lockScroll={false}>
      <Drawer.Content>
        <Drawer.Header className="gap-1">
          {sources && (
            <button type="button" className="net-icon-button" onClick={() => setSources(false)} aria-label="Back to nodes" title="Back to nodes">
              <IconArrowLeft size={16} />
            </button>
          )}
          <Drawer.Title className="mr-auto">{sources ? "Subscriptions" : "Nodes"}</Drawer.Title>
          {!sources && (
            <>
              <button type="button" className="net-icon-button" aria-label="Search nodes" title="Search nodes" aria-expanded={searching}
                onClick={() => { setSearching(!searching); setQuery(""); setCount(PAGE); }}>
                <IconSearch size={16} />
              </button>
              <button type="button" className="net-icon-button" aria-label={testLabel} title={testLabel} disabled={!testable}
                onClick={() => queueNodeTests(visible)}>
                {visible.length > 0 && !testable ? <Loader size={13} /> : <IconBolt size={16} />}
              </button>
              <button type="button" className="net-icon-button" aria-label="Manage subscriptions" title="Manage subscriptions" onClick={() => setSources(true)}>
                <IconLink size={16} />
              </button>
            </>
          )}
          <Drawer.CloseButton aria-label="Close nodes" ml={0} />
        </Drawer.Header>
        <Drawer.Body>
          {searching && !sources && (
            <TextInput autoFocus size="xs" className="mb-2" aria-label="Find a node" placeholder="Find a node" value={query}
              onChange={(event) => { setQuery(event.currentTarget.value); setCount(PAGE); }} />
          )}
          {sources ? <Sources catalog={catalog} hostRefreshAllowed={hostRefreshAllowed} /> : visible.length > 0 ? (
            <>
              <div className="grid grid-cols-[repeat(auto-fill,minmax(min(100%,12rem),1fr))] gap-1.5">
                {visible.slice(0, count).map((node) => (
                  <NodeCard key={nodeKey(node)} node={node} inRoute={inRoute.has(node.id)} onAdd={onAdd} />
                ))}
              </div>
              {count < visible.length && (
                <Button className="mt-2" size="compact-xs" variant="subtle" color="gray" onClick={() => setCount(count + PAGE)}>Show more</Button>
              )}
            </>
          ) : (
            <div className="net-drawer-empty">
              {catalog.nodes.length ? "No nodes match." : <Button size="xs" variant="default" onClick={() => setSources(true)}>Add a subscription</Button>}
            </div>
          )}
        </Drawer.Body>
      </Drawer.Content>
    </Drawer.Root>
  );
}
