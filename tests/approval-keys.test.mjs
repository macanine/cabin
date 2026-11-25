import assert from "node:assert/strict";
import test from "node:test";
import { ARM_MS, answerKey, approvalArm } from "../src/lib/approvalKeys.ts";

const key = (over = {}) => ({ key: "Y", repeat: false, isComposing: false, metaKey: false, ctrlKey: false, altKey: false, ...over });

test("keys pressed while the card appears do not answer it", () => {
  const arm = approvalArm(1000);
  assert.equal(arm.press(1010), false);
  // Continuous typing keeps the card unarmed, however long it goes on.
  for (let t = 1200; t < 3000; t += 200) assert.equal(arm.press(t), false);
});

test("a deliberate key after a quiet moment answers, and stays armed", () => {
  const arm = approvalArm(1000);
  assert.equal(arm.press(1000 + ARM_MS), true);
  assert.equal(arm.press(1001 + ARM_MS), true);
});

test("text fields, shortcuts, key repeat and IME input never answer", () => {
  assert.equal(answerKey(key(), false), "y");
  assert.equal(answerKey(key(), true), null);
  for (const over of [{ repeat: true }, { isComposing: true }, { metaKey: true }, { ctrlKey: true }, { altKey: true }]) {
    assert.equal(answerKey(key(over), false), null);
  }
});
