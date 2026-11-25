// Cache the guest's disk-backed catalog per workspace, session root and agent.
// Live commands come from chat controls, not this request.
import type { AgentCatalog, AgentKind } from "../services/ipc/generated/core";
import { agentsIpc } from "../services/ipc/agents";

const FRESH_MS = 2 * 60 * 1000;
const cache = new Map<string, { at: number; value: Promise<AgentCatalog> }>();

export function loadCatalog(workspaceId: string, agent: AgentKind, force = false, sessionId: string | null = null, worktree: string | null = null): Promise<AgentCatalog> {
  const key = `${JSON.stringify([workspaceId, sessionId, worktree])}:${agent}`;
  const hit = cache.get(key);
  if (hit && !force && Date.now() - hit.at < FRESH_MS) return hit.value;
  const value = agentsIpc.catalog(workspaceId, agent, sessionId).catch((error: unknown) => {
    if (cache.get(key)?.value === value) cache.delete(key);
    throw error;
  });
  cache.set(key, { at: Date.now(), value });
  return value;
}

/** Forget cached answers after skills or MCP servers change. */
export function invalidateCatalogs(agent?: AgentKind) {
  for (const key of [...cache.keys()]) if (!agent || key.endsWith(`:${agent}`)) cache.delete(key);
}
