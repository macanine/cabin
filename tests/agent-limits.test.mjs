import assert from "node:assert/strict";
import test from "node:test";
import { limitLines, shortLimitLabel } from "../src/lib/agentLimits.ts";

test("limit labels are shortened the same way for every agent", () => {
  assert.equal(shortLimitLabel("5h limit"), "5-hour");
  assert.equal(shortLimitLabel("Current session"), "5-hour");
  assert.equal(shortLimitLabel("7-day"), "Weekly");
  assert.equal(shortLimitLabel("Current week (Opus)"), "Weekly Opus");
  assert.equal(shortLimitLabel("Monthly credits"), "Monthly");
  assert.equal(shortLimitLabel("Daily requests"), "Daily requests");
});

test("lines say what is left, and flag what is nearly used up", () => {
  assert.deepEqual(limitLines([
    { label: "5h", usedPercent: 6, resetsAt: 1 },
    { label: "Weekly", usedPercent: 93.6, resetsAt: null },
    { label: "7-day", usedPercent: 120, resetsAt: null },
  ]), [
    { text: "5-hour 94% left", low: false, resetsAt: 1 },
    { text: "Weekly 6% left", low: true, resetsAt: null },
    { text: "Weekly used up", low: true, resetsAt: null },
  ]);
});
