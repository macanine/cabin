import type {
  GeoInfo, IpClassification, NetworkHistoryEntry, NetworkRoute as GeneratedNetworkRoute, NetworkStatus, NetworkTraffic, NodePlace,
  ProbeResult, ProxyCatalog, ProxyTestKind, ProxyTestResult, RouteCheck,
} from "./generated/core";
import { invokeGuest } from "./common";
import { normalizeRoute, type NetworkInterface, type NetworkRoute } from "../../lib/networkRoute";

/** The VM's route and what the dashboard shows about it. The Mac app answers
 * these itself (the gateway and the node catalog live there). */
export const networkIpc = {
  state: () => invokeGuest<NetworkStatus>("network_state"),
  interfaces: () => invokeGuest<NetworkInterface[]>("network_interfaces"),
  apply: (route: GeneratedNetworkRoute) => invokeGuest<NetworkStatus>("network_apply", { route: normalizeRoute(route) }),
  /** Verify the committed route and publish the report. */
  verify: () => invokeGuest<NetworkStatus>("network_verify"),
  /** Manual pause always wins over automatic recovery. */
  setPaused: (paused: boolean) => invokeGuest<NetworkStatus>("network_pause", { paused }),
  setAutoRecover: (enabled: boolean) =>
    invokeGuest<NetworkStatus>("network_auto_recover", { enabled }),
  /** A chain checked hop by hop (the committed route or a draft). */
  check: (route: NetworkRoute) => invokeGuest<RouteCheck>("network_check", { route }),
  /** Where the VM comes out, through the gateway. */
  exit: () => invokeGuest<GeoInfo>("network_exit"),
  exitType: (ip: string, refresh = false) => invokeGuest<IpClassification | null>("network_exit_type", { ip, refresh }),
  traffic: () => invokeGuest<NetworkTraffic | null>("network_traffic"),
  history: (beforeId: number | null, limit: number) =>
    invokeGuest<NetworkHistoryEntry[]>("network_history", { beforeId, limit }),
  /** This Mac's own public address. */
  origin: () => invokeGuest<GeoInfo>("network_origin"),
  locate: (nodeIds: string[]) => invokeGuest<NodePlace[]>("network_locate", { nodeIds }),
};

export const proxyIpc = {
  catalog: () => invokeGuest<ProxyCatalog>("proxy_catalog"),
  importUrl: (name: string, url: string) => invokeGuest<ProxyCatalog>("proxy_import", { name, url, contents: null }),
  importText: (name: string, contents: string) =>
    invokeGuest<ProxyCatalog>("proxy_import", { name, url: null, contents }),
  importFile: async (name: string, path: string) => {
    const { readTextFile } = await import("./files");
    return invokeGuest<ProxyCatalog>("proxy_import", { name, url: null, contents: await readTextFile(path) });
  },
  /** A single proxy URL (http, https, socks5) as a node of its own. */
  addUrl: (url: string) => invokeGuest<ProxyCatalog>("proxy_add_url", { url }),
  refresh: (id: string) => invokeGuest<ProxyCatalog>("proxy_refresh", { id, automatic: true }),
  /** Fetch an existing source URL from this Mac while the internet gate is closed. */
  refreshFromHost: (id: string) =>
    invokeGuest<ProxyCatalog>("proxy_refresh_from_host", { id }),
  remove: (id: string) => invokeGuest<ProxyCatalog>("proxy_remove", { id }),
  probe: (nodeId: string) => invokeGuest<ProbeResult>("proxy_probe", { nodeId }),
  testNode: (nodeId: string, configRevision: string, kind: ProxyTestKind, bindInterface: string | null) =>
    invokeGuest<ProxyTestResult>("proxy_node_test", { nodeId, configRevision, kind, bindInterface }),
  saveGroup: (id: string | null, name: string, nodeIds: string[]) =>
    invokeGuest<ProxyCatalog>("proxy_group_save", { id, name, nodeIds }),
  removeGroup: (id: string) => invokeGuest<ProxyCatalog>("proxy_group_remove", { id }),
};
