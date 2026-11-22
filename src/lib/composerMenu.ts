// The composer's `/` (commands), `$` (skills) and `@` (files) menus.
// Commands and skills come from the agent's catalog; files from the workspace.
import type { AgentCommand } from "../services/ipc/generated/core";

export interface MenuTrigger {
  sigil: "/" | "$" | "@";
  query: string;
  /** Range of the typed token, sigil included, in the composer text. */
  start: number;
  end: number;
}

/** The menu the caret is in, if any. A sigil at the start of a message or
 * after whitespace opens it — `/` and `$` anywhere, as in codeg; a path's
 * slashes never do. */
export function menuTrigger(text: string, caret: number): MenuTrigger | null {
  const before = text.slice(0, caret);
  const match = /(^|\s)([@/$])([^\s@$]*)$/.exec(before);
  if (!match) return null;
  const sigil = match[2] as "/" | "$" | "@";
  const query = match[3];
  const start = caret - query.length - 1;
  const rest = /^\S*/.exec(text.slice(caret))?.[0] ?? "";
  return { sigil, query, start, end: caret + rest.length };
}

function score(item: AgentCommand, query: string): number | null {
  if (!query) return 0;
  const name = item.name.toLowerCase();
  const q = query.toLowerCase();
  if (name === q) return 0;
  if (name.startsWith(q)) return 1;
  if (name.split(/[-_:.\s]/).some((part) => part.startsWith(q))) return 2;
  if (name.includes(q)) return 3;
  if ((item.description ?? "").toLowerCase().includes(q)) return 4;
  return null;
}

/** Entries for a trigger, best match first. `/` lists the agent's commands
 * (including Codex's `$`-named ones) and slash-style skills; `$` lists the
 * dollar-style skills and commands; `@` lists workspace files. */
export function menuItems(commands: AgentCommand[], trigger: MenuTrigger, limit = 40): AgentCommand[] {
  const pool = commands.filter((item) => {
    if (trigger.sigil === "@") return item.kind === "file";
    if (trigger.sigil === "/") {
      return item.kind === "command" || (item.kind === "skill" && item.insert.startsWith("/"));
    }
    return item.kind === "skill" && item.insert.startsWith("$");
  });
  // A live command may repeat a slash skill's token (and merges keep the disk
  // copy of `$` skills); one row per token, the first (the agent's) winning.
  const seen = new Set<string>();
  const unique = pool.filter((item) => {
    const token = item.insert.trim();
    if (seen.has(token)) return false;
    seen.add(token);
    return true;
  });
  return unique
    .map((item) => ({ item, rank: score(item, trigger.query) }))
    .filter((entry): entry is { item: AgentCommand; rank: number } => entry.rank != null)
    .sort((a, b) => a.rank - b.rank || a.item.kind.localeCompare(b.item.kind) || a.item.name.localeCompare(b.item.name))
    .slice(0, limit)
    .map((entry) => entry.item);
}

/** Text to insert for an entry at the trigger's position: the token the agent
 * runs, exactly as its own CLI spells it (`/review`, `$pdf`, `/skill:pdf`). */
export function insertionFor(item: AgentCommand, _trigger: MenuTrigger): string {
  return item.insert;
}

/** Replace the trigger's token with the entry; returns the new text and caret. */
export function applyItem(text: string, trigger: MenuTrigger, item: AgentCommand): { text: string; caret: number } {
  const insert = insertionFor(item, trigger);
  let rest = text.slice(trigger.end);
  if (insert.endsWith(" ") && rest.startsWith(" ")) rest = rest.slice(1);
  const next = text.slice(0, trigger.start) + insert + rest;
  return { text: next, caret: trigger.start + insert.length };
}
