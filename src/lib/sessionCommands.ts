import type { AgentCommand } from "../services/ipc/generated/core";

const entries = [
  { name: "status", insert: "/cabin:status", description: "Show session status" },
  { name: "context", insert: "/cabin:context", description: "Show context window usage" },
] as const;

export type SessionCommand = typeof entries[number]["name"];

export const sessionCommands: AgentCommand[] = entries.map((entry) => ({
  ...entry, insert: `${entry.insert} `, kind: "command", source: "cabin", hint: null,
}));

export function sessionCommandFor(text: string, hasAttachments: boolean): SessionCommand | null {
  if (hasAttachments) return null;
  return entries.find((entry) => entry.insert === text.trim())?.name ?? null;
}
