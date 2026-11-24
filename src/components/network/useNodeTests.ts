import { useCallback, useSyncExternalStore } from "react";
import { guestNow } from "../../lib/clock";
import type { ProxyNode, ProxyTestResult } from "../../services/ipc/generated/core";
import { toIpcError } from "../../services/ipc/common";
import { proxyIpc } from "../../services/ipc/network";
import { appStore } from "../../state/store";
import { nodeKey } from "./nodeCatalog";

type TestState = { pending: boolean; result?: ProxyTestResult };
type Listener = () => void;

const IDLE: TestState = { pending: false };
const CONCURRENCY = 4;
const tests = new Map<string, TestState>();
const nodeListeners = new Map<string, Set<Listener>>();
const queueListeners = new Set<Listener>();
const queue: ProxyNode[] = [];
let nextJob = 0;
let running = 0;
let nodes = appStore.get().proxy.nodes;
let currentKeys = new Set(nodes.map(nodeKey));

function subscribe(listeners: Set<Listener>, listener: Listener) {
  listeners.add(listener);
  return () => { listeners.delete(listener); };
}

function setTest(key: string, value?: TestState) {
  if (value) tests.set(key, value);
  else tests.delete(key);
  nodeListeners.get(key)?.forEach((listener) => listener());
}

appStore.subscribe(() => {
  const next = appStore.get().proxy.nodes;
  if (nodes === next) return;
  nodes = next;
  currentKeys = new Set(nodes.map(nodeKey));
  for (const [key, value] of tests) {
    if (!currentKeys.has(key)) setTest(key, value.pending ? { pending: true } : undefined);
  }
  queueListeners.forEach((listener) => listener());
});

export function useNodeTest(node: ProxyNode | undefined): TestState {
  const key = node ? nodeKey(node) : null;
  const listen = useCallback((listener: Listener) => {
    if (key == null) return () => {};
    const listeners = nodeListeners.get(key) ?? new Set<Listener>();
    nodeListeners.set(key, listeners);
    const unsubscribe = subscribe(listeners, listener);
    return () => {
      unsubscribe();
      if (listeners.size === 0) nodeListeners.delete(key);
    };
  }, [key]);
  return useSyncExternalStore(listen, () => key == null ? IDLE : tests.get(key) ?? IDLE);
}

function subscribeQueue(listener: Listener) {
  return subscribe(queueListeners, listener);
}

export function useCanTestNodes(candidates: readonly ProxyNode[]): boolean {
  return useSyncExternalStore(subscribeQueue, () => candidates.some((node) => !tests.get(nodeKey(node))?.pending));
}

async function run(node: ProxyNode, key: string) {
  let result: ProxyTestResult;
  try {
    result = await proxyIpc.testNode(node.id, node.configRevision, "latency");
  } catch (error) {
    result = { nodeId: node.id, configRevision: node.configRevision, kind: "latency", testedAt: guestNow(), ok: false, latencyMs: null, bytes: null, elapsedMs: null, detail: toIpcError(error).message };
  }
  running--;
  if (currentKeys.has(key)) {
    const valid = result.nodeId === node.id && result.configRevision === node.configRevision && result.kind === "latency";
    setTest(key, { pending: false, result: valid ? result : tests.get(key)?.result });
    queueListeners.forEach((listener) => listener());
  } else {
    setTest(key);
  }
  dispatch();
}

function dispatch() {
  while (running < CONCURRENCY && nextJob < queue.length) {
    const node = queue[nextJob++];
    const key = nodeKey(node);
    if (!currentKeys.has(key)) {
      setTest(key);
      continue;
    }
    running++;
    void run(node, key);
  }
  if (nextJob === queue.length) {
    nextJob = 0;
    queue.length = 0;
  }
}

export function queueNodeTests(candidates: readonly ProxyNode[]) {
  for (const node of candidates) {
    const key = nodeKey(node);
    const previous = tests.get(key);
    if (previous?.pending || !currentKeys.has(key)) continue;
    queue.push(node);
    setTest(key, { pending: true, result: previous?.result });
  }
  queueListeners.forEach((listener) => listener());
  dispatch();
}
