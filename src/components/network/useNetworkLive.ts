import { useEffect, useRef, useState } from "react";
import type { ProxyNode, RouteCheck } from "../../services/ipc/generated/core";
import { routeCheckKey, type NetworkRoute } from "../../lib/networkRoute";
import { toIpcError } from "../../services/ipc/common";
import { checkChain, locate, useNetworkMonitor, type Lookup } from "../../state/networkMonitor";

export { REFRESH_CHOICES, type Lookup } from "../../state/networkMonitor";

const EMPTY: Lookup<never> = { value: null, error: null, loading: false, at: null };

/** Route checks are user initiated. A result belongs to the exact ordered
 * configuration that was tested, even if the draft has since changed. */
export function useNetworkLive(route: NetworkRoute, byId: ReadonlyMap<string, ProxyNode>) {
  const live = useNetworkMonitor();
  const key = routeCheckKey(route, new Map(route.chain.map((id) => [id, byId.get(id)?.configRevision])));
  const [result, setResult] = useState<{ key: string; lookup: Lookup<RouteCheck> } | null>(null);
  const run = useRef(0);
  const check = route.enabled && result?.key === key ? result.lookup : EMPTY;

  useEffect(() => {
    void locate(route.chain);
  }, [key]); // eslint-disable-line react-hooks/exhaustive-deps

  async function recheck() {
    if (!route.enabled || !route.chain.length || check.loading) return;
    const mine = ++run.current;
    setResult({ key, lookup: { ...check, loading: true } });
    try {
      const value = await checkChain(route);
      if (mine === run.current) setResult({ key, lookup: { value, error: null, loading: false, at: Date.now() } });
    } catch (error) {
      if (mine === run.current) setResult({ key, lookup: { value: null, error: toIpcError(error).message, loading: false, at: Date.now() } });
    }
  }

  return { ...live, check, recheck };
}
