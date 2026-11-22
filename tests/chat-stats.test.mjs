import assert from "node:assert/strict";
import test from "node:test";
import {
  cacheHitRate, contextShare, formatCost, formatTokens, mergeUsage, sumUsage, tokensPerSecond, toolCounts, turnMarks,
} from "../src/lib/chatStats.ts";

const usage = (fields) => ({ input: 0, cached: 0, cacheWrite: 0, output: 0, reasoning: 0, generationMs: 0, costMicros: null, contextTokens: null, contextWindow: null, ...fields });
const item = (kind, fields = {}) => ({ id: fields.id ?? Math.random().toString(36), kind, status: "completed", text: "", startedAt: 0, updatedAt: 0, ...fields });

test("session totals add turns; context comes from the latest report", () => {
  const total = sumUsage([
    usage({ input: 1000, cached: 800, output: 100, costMicros: 2000, contextTokens: 1000, contextWindow: 200_000 }),
    usage({ input: 3000, cached: 2900, output: 300, generationMs: 4000, contextTokens: 4000 }),
  ]);
  assert.equal(total.input, 4000);
  assert.equal(total.output, 400);
  assert.equal(total.costMicros, 2000);
  assert.equal(total.contextTokens, 4000);
  assert.equal(total.contextWindow, 200_000);
  assert.equal(cacheHitRate(total), 0.925);
  assert.equal(contextShare(total), 0.02);
});

test("speed needs a real streaming sample", () => {
  assert.equal(tokensPerSecond(usage({ output: 500, generationMs: 10_000 })), 50);
  assert.equal(tokensPerSecond(usage({ output: 500, generationMs: 100 })), null);
  assert.equal(cacheHitRate(usage({})), null);
});

test("formatting", () => {
  assert.equal(formatTokens(950), "950");
  assert.equal(formatTokens(1234), "1.2k");
  assert.equal(formatTokens(12_400), "12k");
  assert.equal(formatTokens(1_250_000), "1.25M");
  assert.equal(formatCost(4200), "<$0.01");
  assert.equal(formatCost(420_000), "$0.42");
  assert.equal(formatCost(null), null);
});

test("usage updates replace their turn; tools and marks follow prompts", () => {
  const merged = mergeUsage(mergeUsage([], { turnId: "a", usage: usage({ output: 1 }) }), { turnId: "a", usage: usage({ output: 5 }) });
  assert.deepEqual(merged.map((t) => t.usage.output), [5]);
  const items = [
    item("user_message", { id: "u1", text: "one", startedAt: 1000 }),
    item("tool"), item("command_execution"), item("reasoning"),
    item("assistant_message", { updatedAt: 9000 }),
    item("user_message", { id: "u2", text: "two", startedAt: 10_000 }),
    item("file_change"), item("error"),
  ];
  const tools = toolCounts(items);
  assert.equal(tools.total, 3);
  assert.equal(tools.byTurn.get("u1"), 2);
  const marks = turnMarks(items);
  assert.deepEqual(marks.map((m) => [m.id, m.repliedAt, m.failed]), [["u1", 9000, false], ["u2", null, true]]);
});
