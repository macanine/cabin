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

function isSubsequence(query: string, value: string): boolean {
  let offset = 0;
  for (const character of query) {
    const index = value.indexOf(character, offset);
    if (index === -1) return false;
    offset = index + character.length;
  }
  return true;
}

function score(item: AgentCommand, query: string): number | null {
  if (!query) return 0;
  const name = item.name.toLowerCase();
  const q = query.toLowerCase();
  const hasSearchableToken = (item.kind === "skill" && item.insert.startsWith("@")) || item.source === "cabin";
  const names = hasSearchableToken ? [name, item.insert.slice(1).trim().toLowerCase()] : [name];
  if (names.some((value) => value === q)) return 0;
  if (names.some((value) => value.startsWith(q))) return 1;
  if (names.some((value) => value.split(/[-_:.\s]/).some((part) => part.startsWith(q)))) return 2;
  if (names.some((value) => value.includes(q))) return 3;
  if (q.length >= 3 && item.kind !== "file" && names.some((value) => isSubsequence(q, value))) return 4;
  if ((item.description ?? "").toLowerCase().includes(q)) return 5;
  return null;
}

/** `/` discovers both commands and skills, without changing how they're sent.
 * `$` is a shortcut for dollar-style skills; `@` lists files. */
export function menuItems(commands: AgentCommand[], trigger: MenuTrigger, limit = 40): AgentCommand[] {
  const pool = commands.filter((item) => {
    if (trigger.sigil === "@") return item.kind === "file";
    if (trigger.sigil === "/") return item.kind === "command" || item.kind === "skill";
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

export function menuKeyAction(key: string, shift: boolean, hasItem: boolean, repeat = false) {
  if (key === "Escape") return repeat ? "hold" : "dismiss";
  if (!shift && key === "ArrowDown") return "next";
  if (!shift && key === "ArrowUp") return "previous";
  if (!shift && (key === "Enter" || (key === "Tab" && hasItem))) return hasItem && !repeat ? "pick" : "hold";
  return null;
}

/** Keep the agent's command or skill reference as provided by the catalog. */
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
