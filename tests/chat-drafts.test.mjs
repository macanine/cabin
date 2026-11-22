import assert from "node:assert/strict";
import test from "node:test";
import { loadDraft, saveDraft, withDraft } from "../src/lib/chatDrafts.ts";

test("a blank draft is removed, others are kept", () => {
  let drafts = withDraft({}, "a", "hello", 1);
  drafts = withDraft(drafts, "b", "world", 2);
  assert.deepEqual(Object.keys(drafts).sort(), ["a", "b"]);
  drafts = withDraft(drafts, "a", "   ", 3);
  assert.deepEqual(Object.keys(drafts), ["b"]);
});

test("only the most recently edited drafts are kept", () => {
  let drafts = {};
  for (let i = 0; i < 5; i++) drafts = withDraft(drafts, `s${i}`, "x", i, 3);
  drafts = withDraft(drafts, "s2", "edited", 10, 3);
  assert.deepEqual(Object.keys(drafts).sort(), ["s2", "s3", "s4"]);
  drafts = withDraft(drafts, "s5", "new", 11, 3);
  assert.deepEqual(Object.keys(drafts).sort(), ["s2", "s4", "s5"]);
});

test("without storage, drafts are simply not remembered", () => {
  assert.equal(loadDraft("a"), "");
  assert.doesNotThrow(() => saveDraft("a", "text"));
});
