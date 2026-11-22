import assert from "node:assert/strict";
import test from "node:test";
import { stackHeight, stackLayout } from "../src/lib/toastStack.ts";

test("collapsed: the newest in front, older ones peek out behind, smaller", () => {
  const slots = stackLayout([40, 40, 40, 40], false);
  assert.deepEqual(slots.map((slot) => slot.y), [0, 9, 18, 27]);
  assert.deepEqual(slots.map((slot) => slot.z), [4, 3, 2, 1]);
  assert.equal(slots[0].scale, 1);
  assert.ok(slots[1].scale < 1 && slots[2].scale < slots[1].scale);
  assert.equal(slots[3].opacity, 0);
  assert.equal(stackHeight([40, 40, 40, 40], false), 58);
});

test("expanded: a column with each toast's own height", () => {
  const slots = stackLayout([40, 60, 40], true);
  assert.deepEqual(slots.map((slot) => slot.y), [0, 48, 116]);
  assert.ok(slots.every((slot) => slot.scale === 1 && slot.opacity === 1));
  assert.equal(stackHeight([40, 60, 40], true), 156);
  assert.equal(stackHeight([], true), 0);
});
