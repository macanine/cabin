import assert from "node:assert/strict";
import test from "node:test";
import {
  EMPTY_CONTROLS, contextFill, currentLabel, groupedChoices, liveCommands, mergeCommands,
  planProgress, queueMessage, sessionOptions, takeQueued,
} from "../src/lib/agentControls.ts";
import { diffCounts, diffRows } from "../src/lib/diffLines.ts";

const option = (fields = {}) => ({
  id: "mode", name: "Session Mode", description: null, category: "mode", kind: "select", current: "build",
  choices: [
    { value: "build", name: "Build", description: null, group: null },
    { value: "plan", name: "Plan", description: null, group: null },
  ],
  ...fields,
});

test("live commands become composer entries and win over the disk catalog", () => {
  const live = liveCommands({ ...EMPTY_CONTROLS, commands: [{ name: "compact", description: "Compact the session", hint: null }] });
  assert.deepEqual(live[0], {
    name: "compact", description: "Compact the session", hint: null, kind: "command", source: "agent", insert: "/compact ",
  });
  const codexSkill = liveCommands({ ...EMPTY_CONTROLS, commands: [{ name: "$deploy", description: null, hint: null }] });
  assert.equal(codexSkill[0].insert, "$deploy ", "a $-named command keeps its own trigger");
  const catalog = [
    { name: "compact", description: "old", hint: null, kind: "command", source: "builtin", insert: "/compact " },
    { name: "pdf", description: null, hint: null, kind: "skill", source: "user", insert: "$pdf " },
  ];
  const merged = mergeCommands(catalog, live);
  assert.equal(merged.length, 2);
  assert.equal(merged.find((entry) => entry.name === "compact").source, "agent");
  const codexLive = liveCommands({ ...EMPTY_CONTROLS, commands: [{ name: "$deploy", description: null, hint: null }] });
  const codexCatalog = [
    { name: "deploy", description: null, hint: null, kind: "skill", source: "user", insert: "$deploy " },
  ];
  assert.equal(mergeCommands(codexCatalog, codexLive).length, 2, "the $ menu keeps the disk skill");
});

test("session options leave out the model and selectors with one choice", () => {
  const controls = {
    ...EMPTY_CONTROLS,
    options: [
      option({ id: "model", category: "model" }),
      option(),
      option({ id: "effort", category: "thought_level", choices: [{ value: "x", name: "X", description: null, group: null }] }),
      { id: "fast", name: "Fast", description: null, category: null, kind: "boolean", current: "true", choices: [] },
    ],
  };
  assert.deepEqual(sessionOptions(controls).map((entry) => entry.id), ["mode", "fast"]);
  assert.equal(currentLabel(option()), "Build");
  assert.equal(currentLabel(controls.options[3]), "On");
  assert.equal(currentLabel(option({ current: "unknown" })), "unknown");
});

test("grouped choices keep the agent's order", () => {
  const groups = groupedChoices(option({
    choices: [
      { value: "a", name: "A", description: null, group: "One" },
      { value: "b", name: "B", description: null, group: "One" },
      { value: "c", name: "C", description: null, group: "Two" },
    ],
  }));
  assert.deepEqual(groups.map((group) => [group.group, group.choices.length]), [["One", 2], ["Two", 1]]);
});

test("plan progress counts done entries and names the current one", () => {
  const plan = [
    { content: "Read", priority: "medium", status: "completed" },
    { content: "Edit", priority: "high", status: "in_progress" },
    { content: "Test", priority: "low", status: "pending" },
  ];
  assert.deepEqual(planProgress(plan), { done: 1, total: 3, current: plan[1] });
  assert.equal(planProgress([plan[0]]).current, null);
});

test("context fill is a clamped share, unknown without a window size", () => {
  assert.equal(contextFill(null), null);
  assert.equal(contextFill({ used: 5, size: 0, costMicros: null, currency: null }), null);
  assert.equal(contextFill({ used: 50, size: 200, costMicros: null, currency: null }), 0.25);
  assert.equal(contextFill({ used: 500, size: 200, costMicros: null, currency: null }), 1);
});

test("a busy agent is steered when it can be, otherwise messages queue in order", () => {
  let queue = queueMessage([], "  first ");
  queue = queueMessage(queue, "second");
  queue = queueMessage(queue, "   ");
  assert.deepEqual(queue.map((entry) => entry.text), ["first", "second"]);
  const [next, rest] = takeQueued(queue);
  assert.equal(next.text, "first");
  assert.deepEqual(rest.map((entry) => entry.text), ["second"]);
  assert.deepEqual(takeQueued([]), [null, []]);
});

test("diff rows carry old and new line numbers per hunk", () => {
  const patch = [
    "--- a/x.txt", "+++ b/x.txt",
    "@@ -3,3 +3,4 @@",
    " c", "-d", "+D", "+E", " f",
    "\\ No newline at end of file",
  ].join("\n");
  const rows = diffRows(patch);
  assert.deepEqual(rows.map((row) => [row.kind, row.old, row.new, row.text]), [
    ["meta", null, null, "--- a/x.txt"],
    ["meta", null, null, "+++ b/x.txt"],
    ["hunk", null, null, "@@ -3,3 +3,4 @@"],
    ["same", 3, 3, "c"],
    ["del", 4, null, "d"],
    ["add", null, 4, "D"],
    ["add", null, 5, "E"],
    ["same", 5, 6, "f"],
    ["note", null, null, "\\ No newline at end of file"],
  ]);
  assert.deepEqual(diffCounts(patch), { added: 2, removed: 1 });
});

import { liveModel } from "../src/lib/agentControls.ts";
import { compareVersions, latestStable, offeredVersions, updateFor } from "../src/lib/runtimeVersions.ts";

test("options the permission picker owns stay hidden; the live model is found", () => {
  const controls = {
    ...EMPTY_CONTROLS,
    options: [
      option({ hidden: true }),
      option({ id: "effort", category: "thought_level", hidden: false }),
      option({ id: "model", category: "model", hidden: false, current: "a", choices: [{ value: "a", name: "A", description: null, group: null }] }),
    ],
  };
  assert.deepEqual(sessionOptions(controls).map((entry) => entry.id), ["effort"]);
  assert.equal(liveModel(controls).id, "model");
  assert.equal(liveModel(EMPTY_CONTROLS), null);
});

test("runtime versions sort like semver and updates ignore previews", () => {
  const published = ["2.6.1", "2.7.1-preview.3", "2.7.0", "2.10.0-preview.1", "2.6.1-preview.10"];
  assert.deepEqual([...published].sort((a, b) => compareVersions(b, a)),
    ["2.10.0-preview.1", "2.7.1-preview.3", "2.7.0", "2.6.1", "2.6.1-preview.10"]);
  assert.equal(latestStable(published), "2.7.0");
  assert.equal(updateFor("2.6.1", published), "2.7.0");
  assert.equal(updateFor("2.7.0", published), null);
  assert.equal(updateFor("2.7.1-preview.3", published), null);
  assert.deepEqual(offeredVersions(published, ["2.5.0", "2.7.1-preview.3"], false), ["2.7.1-preview.3", "2.7.0", "2.6.1", "2.5.0"]);
});
