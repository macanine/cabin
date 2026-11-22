import { useSyncExternalStore } from "react";
import { guestNow } from "../../lib/clock";
import type { ProxyNode, ProxyTestKind, ProxyTestResult } from "../../services/ipc/generated/core";
import { toIpcError } from "../../services/ipc/common";
import { proxyIpc } from "../../services/ipc/network";
import { appStore } from "../../state/store";
import { nodeKey } from "./nodeCatalog";
import { nodeTestKey } from "../../lib/networkRoute";

export interface NodeTestState {
  results: ReadonlyMap<string, ProxyTestResult>;
  pending: ReadonlySet<string>;
}

const listeners = new Set<() => void>();
let state: NodeTestState = { results: new Map(), pending: new Set() };
type TestJob = { node: ProxyNode; bindInterface: string | null };
const queues: Record<ProxyTestKind, TestJob[]> = { latency: [], download: [] };
const running = { latency: 0, download: 0 };
const limits = { latency: 4, download: 1 };
let catalog = appStore.get().proxy;
let currentKeys = new Set(catalog.nodes.map(nodeKey));

export const testKey = nodeTestKey;

function publish(next: NodeTestState) {
  state = next;
  listeners.forEach((listener) => listener());
}

function syncCatalog() {
  const next = appStore.get().proxy;
  if (catalog === next) return;
  catalog = next;
  currentKeys = new Set(catalog.nodes.map(nodeKey));
  const results = new Map([...state.results].filter(([, value]) => currentKeys.has(nodeKey({ id: value.nodeId, configRevision: value.configRevision }))));
  if (results.size !== state.results.size) publish({ ...state, results });
}

appStore.subscribe(syncCatalog);

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => { listeners.delete(listener); };
}

export function useNodeTests(): NodeTestState {
  return useSyncExternalStore(subscribe, () => state);
}

function finish({ node, bindInterface }: TestJob, kind: ProxyTestKind, result?: ProxyTestResult) {
  const key = testKey(node, kind, bindInterface);
  const pending = new Set(state.pending);
  pending.delete(key);
  const valid = result?.nodeId === node.id && result.configRevision === node.configRevision
    && result.kind === kind && currentKeys.has(nodeKey(node));
  const results = valid ? new Map(state.results).set(key, result) : state.results;
  publish({ results, pending });
}

async function run(job: TestJob, kind: ProxyTestKind) {
  const { node, bindInterface } = job;
  let result: ProxyTestResult;
  try {
    result = await proxyIpc.testNode(node.id, node.configRevision, kind, bindInterface);
  } catch (error) {
    result = { nodeId: node.id, configRevision: node.configRevision, kind, testedAt: guestNow(), ok: false, latencyMs: null, bytes: null, elapsedMs: null, detail: toIpcError(error).message };
  }
  running[kind]--;
  finish(job, kind, result);
  dispatch(kind);
}

function dispatch(kind: ProxyTestKind) {
  const queue = queues[kind];
  while (running[kind] < limits[kind] && queue.length > 0) {
    const job = queue.shift()!;
    if (!currentKeys.has(nodeKey(job.node))) {
      finish(job, kind);
      continue;
    }
    running[kind]++;
    void run(job, kind);
  }
}

export function queueNodeTests(nodes: readonly ProxyNode[], kind: ProxyTestKind, bindInterface: string | null) {
  const pending = new Set(state.pending);
  for (const node of nodes) {
    const key = testKey(node, kind, bindInterface);
    if (pending.has(key) || !currentKeys.has(nodeKey(node))) continue;
    pending.add(key);
    queues[kind].push({ node, bindInterface });
  }
  if (pending.size !== state.pending.size) publish({ ...state, pending });
  dispatch(kind);
}
