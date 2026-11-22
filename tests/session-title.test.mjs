import { test } from "node:test";
import assert from "node:assert/strict";
import { autoSessionName, usableSessionTitle } from "../src/lib/sessionTitle.ts";

test("the automatic name mirrors the guest's first-line title", () => {
  assert.equal(autoSessionName("  hello啊  \nsecond line"), "hello啊");
  assert.equal(autoSessionName("\n\n  介绍一下你自己呗，你是什么情况\n"), "介绍一下你自己呗，你是什么情况");
  assert.equal(autoSessionName("   "), null);
  const long = "a".repeat(70);
  assert.equal(autoSessionName(long), `${"a".repeat(60)}…`);
  assert.equal(autoSessionName("a".repeat(60)), "a".repeat(60));
  assert.equal(autoSessionName("a\u0000b"), "ab");
});

test("agent titles are cleaned or rejected", () => {
  assert.equal(usableSessionTitle("  Intro to  Rust \n"), "Intro to Rust");
  assert.equal(usableSessionTitle(""), null);
  assert.equal(usableSessionTitle(null), null);
  assert.equal(usableSessionTitle("a".repeat(81)), null);
  assert.equal(usableSessionTitle("a\nb"), "a b");
});
