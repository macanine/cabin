import assert from "node:assert/strict";
import test from "node:test";
import { rankPalette, scoreItem } from "../src/lib/commandPalette.ts";

const item = (id, label, extra = {}) => ({ id, label, group: "Tasks", ...extra });
const ids = (groups) => groups.flatMap((g) => g.items.map((i) => i.id));

test("an empty query lists everything except search-only items, in order", () => {
  const items = [item("a", "Fix sync"), item("b", "Old", { onSearchOnly: true }), item("c", "Usage", { group: "Go to" })];
  assert.deepEqual(ids(rankPalette(items, "  ")), ["a", "c"]);
  assert.deepEqual(rankPalette(items, "").map((g) => g.group), ["Tasks", "Go to"]);
});

test("word starts rank above inner matches, and labels above hints", () => {
  const items = [item("inner", "resync all"), item("hint", "Build", { hint: "sync" }), item("word", "Fix sync"), item("start", "Sync files")];
  assert.deepEqual(ids(rankPalette(items, "sync")), ["start", "word", "inner", "hint"]);
});

test("every word has to match somewhere", () => {
  const entry = item("a", "Fix sync conflicts", { hint: "api-server", keywords: "codex" });
  assert.ok(scoreItem(entry, "sync server codex") > 0);
  assert.equal(scoreItem(entry, "sync windows"), 0);
});

test("search finds archived items and respects the limit", () => {
  const items = Array.from({ length: 6 }, (_, i) => item(`t${i}`, `Task ${i}`, { onSearchOnly: i === 0 }));
  assert.deepEqual(ids(rankPalette(items, "task", 3)), ["t0", "t1", "t2"]);
});
