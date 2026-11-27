import assert from "node:assert/strict";
import test from "node:test";
import { gridFor, MIN_GRID, settledSender } from "../src/lib/terminal/protocol.ts";

const cell = { width: 7.8, height: 17 };

test("the content area is filled with whole cells", () => {
  assert.deepEqual(gridFor(780, 510, cell), { cols: 100, rows: 30 });
  assert.deepEqual(gridFor(785, 520, cell), { cols: 100, rows: 30 });
});

test("unmeasurable boxes or fonts give no grid; tiny boxes get the minimum", () => {
  assert.equal(gridFor(0, 500, cell), null);
  assert.equal(gridFor(500, 500, { width: 0, height: 17 }), null);
  assert.deepEqual(gridFor(40, 20, cell), MIN_GRID);
});

test("the PTY gets only the settled size once, and reset allows re-sending after disconnect", async () => {
  const sent = [];
  const sender = settledSender((grid) => sent.push(`${grid.cols}x${grid.rows}`), 20);
  // A window drag: many sizes in quick succession.
  for (const cols of [100, 98, 95, 91, 88]) sender.push({ cols, rows: 30 });
  await new Promise((r) => setTimeout(r, 60));
  assert.deepEqual(sent, ["88x30"]);
  // The same size again is not re-sent; flush delivers immediately.
  sender.push({ cols: 88, rows: 30 });
  sender.flush();
  sender.push({ cols: 120, rows: 40 });
  sender.flush();
  assert.deepEqual(sent, ["88x30", "120x40"]);
  // After disconnect reset, the same size is delivered again.
  sender.reset();
  sender.push({ cols: 120, rows: 40 });
  sender.flush();
  assert.deepEqual(sent, ["88x30", "120x40", "120x40"]);
  sender.cancel();
});
