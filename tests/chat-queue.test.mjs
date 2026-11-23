import assert from "node:assert/strict";
import test from "node:test";
import { chatQueue } from "../src/lib/chatQueue.ts";

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
