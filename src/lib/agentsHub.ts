// Shared status and navigation helpers for Agents & providers.
import type { AgentStatus, NodeState } from "../services/ipc/generated/core";
import { majorOf } from "./nodeVersions.ts";

/** What one agent's page shows. */
export const AGENT_SECTIONS = [
  { value: "account", label: "Account" },
  { value: "setup", label: "Setup" },
] as const;
export type AgentSection = (typeof AGENT_SECTIONS)[number]["value"];

export function isAgentSection(value: unknown): value is AgentSection {
  return AGENT_SECTIONS.some((section) => section.value === value);
}

export type AgentTone = "ok" | "info" | "warn" | "bad" | "off";

export interface AgentState {
  tone: AgentTone;
  /** Two or three words: the agent's state at a glance. */
  label: string;
  /** Whether the next step is in Setup (install, repair) rather than Account. */
  needsSetup: boolean;
}

export function currentClaudeAccount(agent: AgentStatus | undefined) {
  const login = agent?.claudeLogin;
  return login?.state === "active" ? login.account : null;
}

export function agentState(agent: AgentStatus | undefined): AgentState {
  if (!agent || !agent.installed) return { tone: "off", label: "Not installed", needsSetup: true };
  if (!agent.cliChecked) return { tone: "off", label: "Checking", needsSetup: false };
  if (!agent.version) return { tone: "bad", label: "Broken install", needsSetup: true };
  if (agent.configError) return { tone: "bad", label: "Config problem", needsSetup: true };
  if (agent.claudeLogin?.state === "unavailable") return { tone: "info", label: "Sign-in check unavailable", needsSetup: false };
  if (agent.authenticated) {
    const label = agent.kind === "claude" && !currentClaudeAccount(agent)?.verified ? "Credentials found" : "Signed in";
    return { tone: "ok", label, needsSetup: false };
  }
  if (agent.kind === "opencode") return { tone: "info", label: "Free providers", needsSetup: false };
  return { tone: "warn", label: "Sign-in needed", needsSetup: false };
}

export function needsCliLogin(agent: AgentStatus): boolean {
  const state = agentState(agent);
  return state.tone === "warn" || (agent.kind === "opencode" && state.tone === "info" && !agent.authenticated);
}

/** Agents that need the person's attention (broken, or not signed in). */
export function attentionCount(agents: readonly AgentStatus[]): number {
  return agents.filter((agent) => {
    const tone = agentState(agent).tone;
    return agent.installed && (tone === "bad" || tone === "warn");
  }).length;
}

/** Why Node.js is unavailable for agent installs that require it. */
export function nodeProblem(node: NodeState | null): string | null {
  if (node == null) return null;
  const active = node.active;
  if (!active) return "This machine has no Node.js yet. Agents installed through npm need it.";
  if ((majorOf(active.version) ?? 0) < node.minimumMajor) return `Node.js ${active.version} is too old for agents installed through npm.`;
  if (!active.npm) return `Node.js ${active.version} has no npm, which some agent installs need.`;
  return null;
}

/** The section to open for an agent when none was asked for. */
export function defaultSection(agent: AgentStatus | undefined): AgentSection {
  return agentState(agent).needsSetup ? "setup" : "account";
}
