import assert from "node:assert/strict";
import test from "node:test";
import {
  actionablePlan, hasOpenPlan, mergePlan, mergePlans, mergeSubagents, planKey, visiblePlans,
} from "../src/lib/chatPlans.ts";

function plan(fields = {}) {
  return {
    planId: "p1",
    agent: "codex",
    sessionId: "s1",
    revision: 1,
    title: "Plan",
    content: "# Plan\nDo it",
    contentHash: "hash",
    complete: true,
    reviewKind: "next_turn",
    nativePlanId: "native",
    execPermission: "auto",
    status: "awaiting_review",
    execution: "not_started",
    decision: null,
    feedback: null,
    decisionIntentAt: null,
    appliedAt: null,
    createdAt: 100,
    updatedAt: 100,
    supersededBy: null,
    ...fields,
  };
}

test("a newer revision supersedes every older open revision", () => {
  const draft = plan({ revision: 0, status: "draft", complete: false });
  const reviewed = plan({ revision: 1 });
  const next = mergePlan([draft], reviewed);
  assert.equal(next.find((entry) => entry.revision === 0).status, "superseded");
  assert.equal(next.find((entry) => entry.revision === 0).supersededBy, 1);
  assert.equal(next.find((entry) => entry.revision === 1).status, "awaiting_review");
});

test("a late open event cannot resurrect an approved revision", () => {
  const approved = plan({ status: "approved" });
  const stale = plan({ status: "awaiting_review", updatedAt: 999 });
  assert.deepEqual(mergePlans([approved], [stale]), [approved]);
});

test("a late older revision cannot appear beside a newer one", () => {
  const newer = plan({ revision: 2, status: "awaiting_review" });
  const older = plan({ revision: 0, status: "draft", complete: false, updatedAt: 999 });
  assert.deepEqual(mergePlan([newer], older), [newer]);
});

test("deferred becomes actionable only after the user resumes it", () => {
  const deferred = plan({ status: "deferred" });
  assert.equal(actionablePlan([deferred], null), null);
  assert.equal(actionablePlan([deferred], planKey(deferred)).status, "deferred");
  assert.equal(actionablePlan([plan({ status: "submitting" })], null).status, "submitting");
  assert.equal(actionablePlan([plan({ status: "draft", complete: false })], null), null);
});

test("open-plan locking includes a deferred native request", () => {
  assert.equal(hasOpenPlan([plan({ status: "deferred" })]), true);
  assert.equal(hasOpenPlan([plan({ status: "approved" })]), false);
  assert.equal(hasOpenPlan([plan({ revision: 0, status: "draft" })]), false);
});

test("an obsolete draft is hidden once its complete revision exists", () => {
  const draft = plan({ revision: 0, status: "superseded", complete: false });
  const reviewed = plan({ revision: 1 });
  assert.deepEqual(visiblePlans([draft, reviewed]).map((entry) => entry.revision), [1]);
});

test("a state-only completion keeps the child transcript", () => {
  const message = { id: "m1", ordinal: 1, revision: 1, updatedAt: 20 };
  const first = {
    id: "a", label: "Scout", subagentType: "explore", detail: "look", state: "active",
    origin: "claude", startedAt: 10, updatedAt: 20, items: [message],
  };
  const done = { ...first, state: "completed", updatedAt: 40, label: "", items: [] };
  const [merged] = mergeSubagents([first], [done]);
  assert.equal(merged.state, "completed");
  assert.equal(merged.label, "Scout");
  assert.deepEqual(merged.items.map((entry) => entry.id), ["m1"]);
});

test("subagent history upserts by id and ignores stale snapshots", () => {
  const first = {
    id: "a", label: "Scout", subagentType: "explore", detail: "look", state: "active",
    origin: "claude", startedAt: 10, updatedAt: 20, items: [],
  };
  const done = { ...first, state: "completed", updatedAt: 30 };
  const stale = { ...first, state: "active", updatedAt: 25 };
  const second = { ...first, id: "b", label: "Reviewer", startedAt: 11, updatedAt: 21 };
  assert.deepEqual(
    mergeSubagents([first], [done, stale, second]).map((task) => [task.id, task.state]),
    [["a", "completed"], ["b", "active"]],
  );
});
