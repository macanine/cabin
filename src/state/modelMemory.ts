// Per-viewer memory of models: the display names the agents listed (so a chip
// can name a model before its list loads) and the last model picked per agent.
import type { AgentKind } from "../services/ipc/generated/core";

const NAMES_KEY = "cabin.modelNames";
const LAST_KEY = "cabin.lastModel";
/** OpenCode lists hundreds of models; only the most recent names are kept. */
const NAME_LIMIT = 400;

function read(key: string): Record<string, string> {
  try {
    const value: unknown = JSON.parse(localStorage.getItem(key) ?? "{}");
    if (!value || typeof value !== "object" || Array.isArray(value)) return {};
    return Object.fromEntries(Object.entries(value).filter((entry): entry is [string, string] => typeof entry[1] === "string"));
  } catch {
    return {};
  }
}

function write(key: string, value: Record<string, string>) {
  try {
    localStorage.setItem(key, JSON.stringify(value));
  } catch {
    // Not remembered in this viewer.
  }
}

let names: Record<string, string> | null = null;
const nameKey = (agent: AgentKind, id: string) => `${agent}:${id}`;

export function rememberModelNames(agent: AgentKind, models: { id: string; name: string }[]) {
  const known = (names ??= read(NAMES_KEY));
  const fresh = models.filter(({ id, name }) => name && name !== id && known[nameKey(agent, id)] !== name);
  if (!fresh.length) return;
  for (const { id, name } of fresh) {
    delete known[nameKey(agent, id)]; // re-added last, so the newest survive the limit
    known[nameKey(agent, id)] = name;
  }
  names = Object.fromEntries(Object.entries(known).slice(-NAME_LIMIT));
  write(NAMES_KEY, names);
}

export function modelName(agent: AgentKind, id: string): string | null {
  return (names ??= read(NAMES_KEY))[nameKey(agent, id)] ?? null;
}

/** The model last picked for this agent. Never chosen for the user: only what they picked. */
export function lastModel(agent: AgentKind): string | null {
  return read(LAST_KEY)[agent] ?? null;
}

export function rememberModel(agent: AgentKind, model: string) {
  write(LAST_KEY, { ...read(LAST_KEY), [agent]: model });
}

export function forgetModel(agent: AgentKind) {
  const all = read(LAST_KEY);
  delete all[agent];
  write(LAST_KEY, all);
}
