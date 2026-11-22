import type { ProxyCatalog, ProxyNode } from "../../services/ipc/generated/core";

export type NodeGroup = `source:${string}` | `group:${string}`;

export function groupNodes(catalog: ProxyCatalog, group: NodeGroup | null): ProxyNode[] {
  if (!group) return catalog.nodes;
  const id = group.slice(group.indexOf(":") + 1);
  if (group.startsWith("source:")) return catalog.nodes.filter((node) => node.sourceId === id);
  const memberIds = new Set(catalog.groups.find((item) => item.id === id)?.nodeIds);
  return catalog.nodes.filter((node) => memberIds.has(node.id));
}

export function nodeMatches(node: ProxyNode, needle: string): boolean {
  return `${node.name} ${node.protocol} ${node.sourceName}`.toLowerCase().includes(needle);
}

export function nodeKey(node: Pick<ProxyNode, "id" | "configRevision">): string {
  return `${node.id}:${node.configRevision}`;
}
