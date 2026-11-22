import { formatCount } from "./format.ts";
/**
 * Chat statistics: token totals, cache hit rate, generation speed, tool
 * counts and per-turn timing. Pure functions over backend usage and items.
 */
import type { ChatItem, ChatTurnUsage, ChatUsage } from "../services/ipc/generated/core";

export const EMPTY_USAGE: ChatUsage = {
  input: 0, cached: 0, cacheWrite: 0, output: 0, reasoning: 0, generationMs: 0,
  costMicros: null, contextTokens: null, contextWindow: null,
};

/** Sum turns; context fields come from the latest turn that reported them. */
export function sumUsage(turns: readonly ChatUsage[]): ChatUsage {
  const total = { ...EMPTY_USAGE };
  for (const usage of turns) {
    total.input += usage.input;
    total.cached += usage.cached;
    total.cacheWrite += usage.cacheWrite;
    total.output += usage.output;
    total.reasoning += usage.reasoning;
    total.generationMs += usage.generationMs;
    if (usage.costMicros != null) total.costMicros = (total.costMicros ?? 0) + usage.costMicros;
    if (usage.contextTokens != null) total.contextTokens = usage.contextTokens;
    if (usage.contextWindow != null) total.contextWindow = usage.contextWindow;
  }
  return total;
}

export function usageByTurn(turns: readonly ChatTurnUsage[]): Map<string, ChatUsage> {
  return new Map(turns.map((turn) => [turn.turnId, turn.usage]));
}

export function mergeUsage(current: ChatTurnUsage[], next: ChatTurnUsage): ChatTurnUsage[] {
  const index = current.findIndex((turn) => turn.turnId === next.turnId);
  if (index < 0) return [...current, next];
  const copy = [...current];
  copy[index] = next;
  return copy;
}

/** Share of prompt tokens served from the cache (0–1), or null. */
export function cacheHitRate(usage: ChatUsage): number | null {
  return usage.input > 0 ? Math.min(1, usage.cached / usage.input) : null;
}

/** Output tokens per second of streaming, or null below a meaningful sample. */
export function tokensPerSecond(usage: ChatUsage): number | null {
  if (usage.output <= 0 || usage.generationMs < 300) return null;
  return usage.output / (usage.generationMs / 1000);
}

export function contextShare(usage: ChatUsage): number | null {
  if (!usage.contextTokens || !usage.contextWindow) return null;
  return Math.min(1, usage.contextTokens / usage.contextWindow);
}

export function hasUsage(usage: ChatUsage): boolean {
  return usage.input > 0 || usage.output > 0;
}

/** 950, 1.2k, 12k, 1.25M, 3.4B (see `formatCount`). */
export function formatTokens(value: number): string {
  return formatCount(value);
}

export function formatCost(micros: number | null): string | null {
  if (micros == null) return null;
  const usd = micros / 1e6;
  if (usd === 0) return "$0";
  if (usd < 0.01) return "<$0.01";
  return `$${usd.toFixed(usd < 10 ? 2 : 1)}`;
}

export function formatPercent(share: number | null): string {
  return share == null ? "—" : `${Math.round(share * 100)}%`;
}

const TOOL_KINDS = new Set<ChatItem["kind"]>(["tool", "command_execution", "file_change"]);

export function isToolCall(item: ChatItem): boolean {
  return TOOL_KINDS.has(item.kind);
}

/** Tool calls per turn (keyed by the user message that started it) and in total. */
export function toolCounts(items: readonly ChatItem[]): { byTurn: Map<string, number>; total: number } {
  const byTurn = new Map<string, number>();
  let turn = "start";
  let total = 0;
  for (const item of items) {
    if (item.kind === "user_message") turn = item.turnId || item.id;
    else if (item.turnId) turn = item.turnId;
    if (isToolCall(item)) {
      byTurn.set(turn, (byTurn.get(turn) ?? 0) + 1);
      total += 1;
    }
  }
  return { byTurn, total };
}

export interface TurnMark {
  id: string;
  prompt: string;
  sentAt: number;
  repliedAt: number | null;
  failed: boolean;
}

/** One mark per prompt for the timeline rail: when it was sent and answered. */
export function turnMarks(items: readonly ChatItem[]): TurnMark[] {
  const marks: TurnMark[] = [];
  let current: TurnMark | null = null;
  for (const item of items) {
    if (item.kind === "user_message") {
      current = { id: item.turnId || item.id, prompt: item.text, sentAt: item.startedAt, repliedAt: null, failed: item.status === "failed" };
      marks.push(current);
    } else if (item.turnId && item.turnId !== current?.id) {
      current = marks.find((mark) => mark.id === item.turnId) ?? null;
      if (current) {
        if (item.kind === "assistant_message" && item.status !== "running") current.repliedAt = item.updatedAt || null;
        if (item.kind === "error") current.failed = true;
      }
    } else if (current) {
      if (item.kind === "assistant_message" && item.status !== "running") current.repliedAt = item.updatedAt || null;
      if (item.kind === "error") current.failed = true;
    }
  }
  return marks;
}
