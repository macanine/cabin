import test from "node:test";
import assert from "node:assert/strict";
import { patchStats, toHunk } from "../src/lib/patch.ts";

const body = "@@ -1,2 +1,2 @@\n a\n-b\n+c\n";

test("a bare hunk gets file lines", () => {
  assert.equal(toHunk(body, "src/a.ts"), "--- a/src/a.ts\n+++ b/src/a.ts\n@@ -1,2 +1,2 @@\n a\n-b\n+c");
});

test("git preamble is dropped and existing file lines are kept", () => {
  const patch = `diff --git a/x b/x\nindex 1..2 100644\n--- a/x\n+++ b/x\n${body}`;
  assert.equal(toHunk(patch, "x"), `--- a/x\n+++ b/x\n${body.trimEnd()}`);
});

test("added and deleted files use /dev/null", () => {
  assert.match(toHunk(body, "n.ts", "added"), /^--- \/dev\/null\n\+\+\+ b\/n\.ts/);
  assert.match(toHunk(body, "n.ts", "deleted"), /^--- a\/n\.ts\n\+\+\+ \/dev\/null/);
});

test("a patch without hunks is empty", () => {
  assert.equal(toHunk("Binary files differ", "x"), "");
});

test("stats ignore the file header lines", () => {
  assert.deepEqual(patchStats(`--- a/x\n+++ b/x\n${body}`), { additions: 1, deletions: 1 });
});
