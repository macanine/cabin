import test from "node:test";
import assert from "node:assert/strict";
import {
  goalBudget, goalLabel, goalTone, latestStep, orderTasks, subagentElapsed, subagentSteps, taskTone, tasksByItem,
} from "../src/lib/chatTasks.ts";

const task = (id, state, fields = {}) => ({
  id, name: id, taskType: "shell", description: "", state, canStop: true, summary: null, lastToolName: null,
  totalTokens: null, toolUses: null, durationMs: null, outputFilePath: null, toolCallId: null, itemId: null,
  startedAt: 0, updatedAt: 0, ...fields,
});
const item = (kind, fields = {}) => ({ id: Math.random().toString(36), kind, status: "completed", title: "", text: "", subject: "", ...fields });

test("background tasks: running first, then the latest finished", () => {
  const { active, finished } = orderTasks([
    task("a", "completed", { updatedAt: 5 }), task("b", "running", { startedAt: 3 }),
    task("c", "failed", { updatedAt: 9 }), task("d", "running", { startedAt: 1 }),
  ], 1);
  assert.deepEqual(active.map((t) => t.id), ["d", "b"]);
  assert.deepEqual(finished.map((t) => t.id), ["c"]);
  assert.equal(taskTone("running"), "live");
  assert.equal(taskTone("failed"), "bad");
  assert.equal(taskTone("paused"), "idle");
});

test("tasks map to the tool card that started them, newest wins", () => {
  const map = tasksByItem([task("x", "completed", { itemId: "i1", startedAt: 1 }), task("y", "running", { itemId: "i1", startedAt: 2 }), task("z", "running")]);
  assert.equal(map.get("i1").id, "y");
  assert.equal(map.size, 1);
});

test("subagent progress: elapsed, latest step and step count", () => {
  assert.equal(subagentElapsed({ state: "active", startedAt: 10, updatedAt: 20 }, 50), 40);
  assert.equal(subagentElapsed({ state: "completed", startedAt: 10, updatedAt: 20 }, 50), 10);
  const items = [item("user_message", { text: "go" }), item("tool", { subject: "src/a.ts" }), item("assistant_message", { text: "All done\nmore" })];
  assert.equal(latestStep(items), "All done");
  assert.equal(latestStep(items.slice(0, 2)), "src/a.ts");
  assert.equal(subagentSteps(items), 1);
});

test("goal states read plainly", () => {
  assert.equal(goalLabel("budget_limited"), "Budget reached");
  assert.equal(goalLabel("some_new_state"), "some new state");
  assert.equal(goalTone("budget_limited"), "bad");
  assert.equal(goalTone("complete"), "ok");
  assert.equal(goalBudget({ tokensUsed: 50, tokenBudget: 200 }), 0.25);
  assert.equal(goalBudget({ tokensUsed: 50, tokenBudget: null }), null);
});
