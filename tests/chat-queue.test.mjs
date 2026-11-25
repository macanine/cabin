import assert from "node:assert/strict";
import test from "node:test";
import { chatQueue, restoreQueues } from "../src/lib/chatQueue.ts";

test("a failed delivery stays in its session and waits for an explicit retry", () => {
  const session = "retry";
  chatQueue.add(session, "first");
  chatQueue.add(session, "second");
  const [first, second] = chatQueue.get(session).messages;
  assert.equal(chatQueue.start(session, first.id), true);
  assert.equal(chatQueue.start(session, first.id), false);
  assert.deepEqual(chatQueue.get("another-session").messages, []);

  chatQueue.finish(session, first.id, false);
  assert.deepEqual(chatQueue.get(session).messages, [first, second]);
  assert.deepEqual(chatQueue.get(session).failed, [first.id]);
  assert.equal(chatQueue.start(session, first.id), false);
  assert.equal(chatQueue.start(session, second.id), false);
  assert.equal(chatQueue.start(session, second.id, true), true);
  chatQueue.finish(session, second.id, false);

  assert.deepEqual(chatQueue.get(session).failed, [first.id, second.id]);
  assert.equal(chatQueue.start(session, first.id, true), true);
  chatQueue.finish(session, first.id, true);
  assert.deepEqual(chatQueue.get(session).failed, [second.id]);
  assert.equal(chatQueue.start(session, second.id), false);
  assert.equal(chatQueue.start(session, second.id, true), true);
  chatQueue.finish(session, second.id, true);
  assert.deepEqual(chatQueue.get(session).messages, []);
});

test("direct and queued sends share a per-session claim", () => {
  const session = "direct";
  const finishDirect = chatQueue.startDirect(session);
  assert.equal(typeof finishDirect, "function");
  assert.equal(chatQueue.startDirect(session), null);
  chatQueue.add(session, "follow-up");
  const [message] = chatQueue.get(session).messages;
  assert.equal(chatQueue.start(session, message.id, true), false);
  finishDirect();

  assert.equal(chatQueue.start(session, message.id), true);
  finishDirect();
  assert.equal(chatQueue.startDirect(session), null);
  chatQueue.remove(session, message.id);
  assert.deepEqual(chatQueue.get(session).messages, [message]);
  chatQueue.finish(session, message.id, true);
  assert.deepEqual(chatQueue.get(session).messages, []);
});

test("a restored queue keeps its order and sends nothing by itself", () => {
  const restored = restoreQueues(JSON.stringify({ s1: [{ id: "a", text: "first" }, { id: "b", text: "second", extra: 1 }], empty: [] }));
  assert.deepEqual([...restored.keys()], ["s1"]);
  assert.deepEqual(restored.get("s1"), {
    messages: [{ id: "a", text: "first" }, { id: "b", text: "second" }], active: null, failed: ["a", "b"],
  });
});

test("unreadable saved queues are ignored", () => {
  for (const saved of [null, "", "not json", "[]", "null", '{"s":1}', '{"s":[null,{"id":1,"text":"x"},{"id":"a","text":""}]}']) {
    assert.equal(restoreQueues(saved).size, 0);
  }
});

test("a deleted session drops its queue", () => {
  chatQueue.add("gone", "later");
  chatQueue.forget("gone");
  assert.deepEqual(chatQueue.get("gone").messages, []);
});
