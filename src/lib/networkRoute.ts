import type { NetworkRoute as GeneratedNetworkRoute, ProxyNode, ProxyTestKind } from "../services/ipc/generated/core";

export type NetworkRoute = GeneratedNetworkRoute;

export interface NetworkInterface {
  name: string;
  label: string;
  kind: string;
  available: boolean;
  ipv4: string[];
}

export function normalizeRoute(route: GeneratedNetworkRoute): NetworkRoute {
  return { ...route, bindInterface: (route as Partial<NetworkRoute>).bindInterface ?? null };
}

export function interfaceOptions(interfaces: readonly NetworkInterface[], selected: string | null) {
  const choices = interfaces.filter((item) => item.name === selected || ["wifi", "ethernet", "hardware"].includes(item.kind)).map((item) => ({
    value: item.name,
    label: `${item.label || item.name}${item.label && item.label !== item.name && !item.label.endsWith(` (${item.name})`) ? ` (${item.name})` : ""}${item.available ? "" : " · Unavailable"}`,
    disabled: !item.available,
  }));
  if (selected && !choices.some((item) => item.value === selected)) {
    choices.push({ value: selected, label: `${selected} · Unavailable`, disabled: true });
  }
  return choices;
}

export function routeCheckKey(route: NetworkRoute, revisions: ReadonlyMap<string, string | undefined>): string {
  return JSON.stringify([route.enabled, route.bindInterface ?? null, route.chain.map((id) => [id, revisions.get(id)])]);
}

export function nodeTestKey(node: Pick<ProxyNode, "id" | "configRevision">, kind: ProxyTestKind, bindInterface: string | null): string {
  return JSON.stringify([kind, node.id, node.configRevision, bindInterface]);
}
