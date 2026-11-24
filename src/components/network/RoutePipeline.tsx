import { Popover, TextInput, Tooltip } from "@mantine/core";
import { IconDeviceLaptop, IconGripVertical, IconPlus, IconSearch, IconWorld, IconX } from "@tabler/icons-react";
import { useId, useLayoutEffect, useMemo, useRef, useState, type DragEvent, type ReactNode } from "react";
import type { ProxyNode, RouteCheck } from "../../services/ipc/generated/core";
import { flag, flagInName, stripFlags, hopRole, insertAt, moveItem, placeLabel, type HopPlace, type LinkState } from "../../lib/geo";
import { nodeKey, nodeMatches } from "./nodeCatalog";
import { useNodeTest } from "./useNodeTests";
import { Flag } from "../Flag";

/** Drag payloads: a node from the drawer, or a hop being reordered. */
export const NODE_DRAG = "application/x-cabin-node";
const HOP_DRAG = "application/x-cabin-hop";

/** The node's name without a leading flag (the flag is shown on its own). */
export function bareName(name: string): string {
  return stripFlags(name);
}

function NodePicker({ nodes, exclude, onPick, children }: {
  nodes: ProxyNode[];
  exclude: ReadonlySet<string>;
  onPick: (id: string) => void;
  children: (open: () => void) => ReactNode;
}) {
  const [opened, setOpened] = useState(false);
  return (
    <Popover opened={opened} onChange={setOpened} position="bottom" withinPortal shadow="md" width={280} trapFocus>
      <Popover.Target>{children(() => setOpened((v) => !v))}</Popover.Target>
      <Popover.Dropdown className="net-picker">
        {/* Searched only while the picker is open. */}
        <PickerList nodes={nodes} exclude={exclude} onPick={(id) => { onPick(id); setOpened(false); }} />
      </Popover.Dropdown>
    </Popover>
  );
}

function PickerList({ nodes, exclude, onPick }: { nodes: ProxyNode[]; exclude: ReadonlySet<string>; onPick: (id: string) => void }) {
  const [query, setQuery] = useState("");
  const shown = useMemo(() => {
    const needle = query.trim().toLowerCase();
    const found: ProxyNode[] = [];
    for (const n of nodes) {
      if (exclude.has(n.id) || !nodeMatches(n, needle)) continue;
      found.push(n);
      if (found.length === 80) break;
    }
    return found;
  }, [nodes, exclude, query]);
  return (
    <>
      <TextInput size="xs" autoFocus leftSection={<IconSearch size={13} />} placeholder="Find a node" value={query} onChange={(e) => setQuery(e.currentTarget.value)} />
      <div className="net-picker-list">
        {shown.length === 0 && <div className="faint fs-12 net-picker-empty">{nodes.length ? "No node matches" : "Import a subscription in Nodes first"}</div>}
        {shown.map((node) => (
          <button key={node.id} className="net-picker-item" onClick={() => onPick(node.id)}>
            <span className="net-flag-sm"><Flag code={flagInName(node.name)} size={11} /></span>
            <span className="truncate grow">{bareName(node.name)}</span>
            <span className="faint fs-12 truncate">{node.sourceName}</span>
          </button>
        ))}
      </div>
    </>
  );
}

/** The divider between two segments: a chevron in the link's colour; on
 * hover it offers + to insert, and it takes drops. */
function Joint({ state, index, nodes, chain, onInsert, onDropAt }: {
  state: LinkState;
  index: number;
  nodes: ProxyNode[];
  chain: string[];
  onInsert: (index: number, id: string) => void;
  onDropAt: (index: number, event: DragEvent) => void;
}) {
  const [over, setOver] = useState(false);
  return (
    <div
      className={`net-joint ${state}${over ? " over" : ""}`}
      onDragOver={(event) => { event.preventDefault(); setOver(true); }}
      onDragLeave={() => setOver(false)}
      onDrop={(event) => { setOver(false); onDropAt(index, event); }}
    >
      <svg className="net-joint-mark" viewBox="0 0 10 20" aria-hidden>
        <path d="M2.5 3.5 L7.5 10 L2.5 16.5" />
      </svg>
      {state === "bad" && <span className="net-joint-break" aria-label="Broken link"><IconX size={9} stroke={3} /></span>}
      <NodePicker nodes={nodes} exclude={new Set(chain)} onPick={(id) => onInsert(index, id)}>
        {(open) => (
          <button className="net-joint-add" onClick={open} aria-label={`Insert a node at position ${index + 1}`}>
            <IconPlus size={11} stroke={2.4} />
          </button>
        )}
      </NodePicker>
    </div>
  );
}

function HopTip({ name, role, place, latency, prefixLatency, failure }: {
  name: string;
  role: string;
  place: HopPlace | undefined;
  latency: number | null;
  prefixLatency: boolean;
  failure: string | null;
}) {
  const where = place?.geo ? placeLabel(place.geo) : "Location unknown";
  return (
    <div className="net-hop-tip">
      <strong>{name}</strong>
      <span>{role}</span>
      <span>{where}</span>
      {place?.host && <span className="mono">{place.host}</span>}
      {failure ? <span className="bad">{failure}</span> : latency != null && <span>{latency} ms · {prefixLatency ? "Chain through this hop" : "Node on its own"}</span>}
    </div>
  );
}

function HopButton({ node, place, selected, position, total, chainLatency, failure, onSelect, onMove }: {
  node?: ProxyNode;
  place: HopPlace | undefined;
  selected: boolean;
  position: number;
  total: number;
  chainLatency?: number | null;
  failure: string | null;
  onSelect: () => void;
  onMove: (offset: number, button: HTMLButtonElement) => void;
}) {
  const { result } = useNodeTest(chainLatency == null ? node : undefined);
  const latency = chainLatency ?? result?.latencyMs ?? null;
  const name = node ? bareName(node.name) : "Missing node";
  const role = hopRole(position - 1, total);
  const mark = flag(place?.geo?.countryCode) ?? (node ? flagInName(node.name) : null);
  return (
    <Tooltip label={<HopTip name={name} role={role} place={place} latency={latency} prefixLatency={chainLatency != null} failure={failure} />}
      openDelay={350} multiline events={{ hover: true, focus: true, touch: false }} maw={300}>
      <button type="button" className="net-seg-select" aria-label={`${name}, ${role.toLowerCase()}, position ${position} of ${total}`}
        aria-pressed={selected} aria-keyshortcuts="ArrowLeft ArrowRight" onClick={onSelect}
        onKeyDown={(event) => {
          if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
          event.preventDefault();
          onMove(event.key === "ArrowLeft" ? -1 : 1, event.currentTarget);
        }}>
        <IconGripVertical size={12} className="net-seg-grip" />
        <span className="net-flag-sm" aria-hidden><Flag code={mark} size={11} /></span>
        <span className="net-seg-name truncate">{name}</span>
        {(failure || latency != null) && (
          <span className={`net-seg-ms${failure ? " bad" : ""}`}>{failure ? "Down" : latency}<small>{failure ? "" : "ms"}</small></span>
        )}
      </button>
    </Tooltip>
  );
}

export function RoutePipeline({ chain, enabled, byId, candidates, places, check, links, onChange, selected, onSelect, draft }: {
  selected: string;
  onSelect: (key: string) => void;
  chain: string[];
  enabled: boolean;
  draft: boolean;
  byId: ReadonlyMap<string, ProxyNode>;
  candidates: ProxyNode[];
  places: ReadonlyMap<string, HopPlace>;
  check: RouteCheck | null;
  links: LinkState[];
  onChange: (chain: string[]) => void;
}) {
  const [dragging, setDragging] = useState<string | null>(null);
  const [announcement, setAnnouncement] = useState("");
  const restoreFocus = useRef<HTMLButtonElement | null>(null);
  const helpId = useId();
  useLayoutEffect(() => {
    restoreFocus.current?.focus({ preventScroll: true });
    restoreFocus.current = null;
  }, [chain]);
  const exclude = useMemo(() => new Set(chain), [chain]);
  const keys = useMemo(() => new Map([...byId.values()].map((node) => [nodeKey(node), node.id])), [byId]);
  const brokenAt = check?.brokenAt ?? null;

  function moveHop(from: number, to: number, button?: HTMLButtonElement) {
    if (from < 0 || to < 0 || to >= chain.length || from === to) return;
    if (button) restoreFocus.current = button;
    const name = byId.get(chain[from])?.name;
    onChange(moveItem(chain, from, to));
    setAnnouncement(`${name ? bareName(name) : "Hop"} moved to position ${to + 1} of ${chain.length}`);
  }

  function dropAt(index: number, event: DragEvent, before = true) {
    event.preventDefault();
    const hop = event.dataTransfer.getData(HOP_DRAG);
    const node = keys.get(event.dataTransfer.getData(NODE_DRAG));
    if (hop) {
      const from = chain.indexOf(hop);
      moveHop(from, before && from < index ? index - 1 : index);
    } else if (node) {
      onChange(insertAt(chain, index, node));
    }
    setDragging(null);
  }

  function insert(index: number, id: string) {
    onChange(insertAt(chain, index, id));
  }

  return (
    <>
      <div className={`net-track${enabled ? "" : " off"}`} role="group" aria-label={draft ? "Draft route" : "Active route"} aria-describedby={chain.length > 1 ? helpId : undefined}>
        <button type="button" className={`net-seg nic${selected === "origin" ? " selected" : ""}`} aria-pressed={selected === "origin"} onClick={() => onSelect("origin")}>
          <IconDeviceLaptop size={15} />
          <span className="net-seg-name">This computer</span>
        </button>
        {chain.map((id, index) => {
          const node = byId.get(id);
          const place = places.get(id);
          const state = enabled ? links[index] ?? "idle" : "idle";
          const hopCheck = check?.hops[index]?.nodeId === id ? check.hops[index] : null;
          const failed = state === "bad";
          const untested = brokenAt != null && index > brokenAt;
          return (
            <div key={id} className="net-seg-slot">
              <Joint state={state} index={index} nodes={candidates} chain={chain} onInsert={insert} onDropAt={dropAt} />
              <div
                className={`net-seg hop ${state}${dragging === id ? " dragging" : ""}${node ? "" : " missing"}${untested ? " untested" : ""}${selected === id ? " selected" : ""}`}
                draggable
                onDragStart={(event) => {
                  event.dataTransfer.setData(HOP_DRAG, id);
                  event.dataTransfer.effectAllowed = "move";
                  setDragging(id);
                }}
                onDragEnd={() => setDragging(null)}
                onDragOver={(event) => event.preventDefault()}
                onDrop={(event) => dropAt(index, event, false)}
              >
                <HopButton node={node} place={place} selected={selected === id}
                  position={index + 1} total={chain.length} chainLatency={hopCheck?.latencyMs} failure={failed ? hopCheck?.detail ?? "Unreachable" : null}
                  onSelect={() => onSelect(id)} onMove={(offset, button) => moveHop(index, index + offset, button)} />
                <button
                  type="button"
                  className="net-seg-remove"
                  aria-label={`Remove ${name} from the route`}
                  onClick={() => {
                    if (selected === id) onSelect("exit");
                    onChange(chain.filter((hop) => hop !== id));
                  }}
                >
                  <IconX size={10} />
                </button>
              </div>
            </div>
          );
        })}
        <div className="net-seg-slot end" onDragOver={(event) => event.preventDefault()} onDrop={(event) => dropAt(chain.length, event)}>
          {chain.length > 0 && (
            <svg className="net-joint-mark idle" viewBox="0 0 10 20" aria-hidden>
              <path d="M2.5 3.5 L7.5 10 L2.5 16.5" />
            </svg>
          )}
          <NodePicker nodes={candidates} exclude={exclude} onPick={(id) => insert(chain.length, id)}>
            {(open) => (
              <Tooltip label="Add a node at the end of the chain" openDelay={300} disabled={chain.length === 0}>
                <button type="button" className={`net-seg add${chain.length ? "" : " wide"}`} onClick={open} aria-label="Add a node at the end of the route">
                  <IconPlus size={13} />
                  {chain.length === 0 && <span>Add a node</span>}
                </button>
              </Tooltip>
            )}
          </NodePicker>
        </div>
        <button type="button" className={`net-seg exit${selected === "exit" ? " selected" : ""}`} aria-pressed={selected === "exit"} onClick={() => onSelect("exit")}>
          <IconWorld size={15} aria-hidden />
          <span className="net-seg-name">Exit</span>
        </button>
      </div>
      {chain.length > 1 && <p className="net-chain-note" id={helpId}>Drag hops or press ← and → on a focused hop to reorder.</p>}
      <span className="net-sr-only" role="status">{announcement}</span>
    </>
  );
}
