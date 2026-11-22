import assert from "node:assert/strict";
import test from "node:test";
import { TerminalReplay } from "../src/lib/terminalReplay.ts";

const running = { state: "running", pid: 1 };
const exited = { state: "exited", code: 0 };
const output = (generation, offset, data) => ({ type: "sessionOutput", id: "terminal", generation, offset, data });
const status = (generation, state) => ({ type: "terminalStatus", id: "terminal", generation, status: state });
const snapshot = (generation, startOffset, data, state = running) => ({
  generation, status: state, startOffset, endOffset: startOffset + Buffer.byteLength(data), data,
});

test("a snapshot and overlapping live output are applied once using UTF-8 offsets", () => {
  const replay = new TerminalReplay();
  assert.deepEqual(replay.snapshot(snapshot(3, 30, "é🙂")), { reset: true, data: "é🙂" });
  assert.equal(replay.event(output(3, 30, "é")), "");
  assert.equal(replay.event(output(3, 32, "🙂\uFEFFx")), "\uFEFFx");
  assert.equal(replay.event(output(3, 40, "!")), "!");
  assert.deepEqual(replay.snapshot(snapshot(3, 32, "🙂\uFEFFx!?")), { reset: false, data: "?" });
  assert.equal(replay.event(output(3, 41, "?")), "");
});

test("a stream gap can be filled from a snapshot and a trimmed gap rebuilds the emulator", () => {
  const replay = new TerminalReplay();
  replay.snapshot(snapshot(2, 0, "one"));
  const live = output(2, 6, "three");
  assert.equal(replay.event(live), null);
  assert.deepEqual(replay.snapshot(snapshot(2, 0, "onetwo")), { reset: false, data: "two" });
  assert.equal(replay.event(live), "three");
  assert.deepEqual(replay.snapshot(snapshot(2, 500, "retained tail")), { reset: true, data: "retained tail" });
  assert.equal(replay.event(output(2, 513, "!")), "!");
});

test("late notifications cannot rewind an exited or replaced shell", () => {
  const replay = new TerminalReplay();
  replay.snapshot(snapshot(1, 0, "old "));
  replay.event(status(1, exited));
  replay.event(status(1, running));
  replay.snapshot(snapshot(1, 0, "old ", running));
  assert.equal(replay.status, exited);
  assert.equal(replay.event(status(2, running)), null);
  assert.deepEqual(replay.snapshot(snapshot(2, 0, "old new")), { reset: true, data: "old new" });
  assert.equal(replay.event(output(1, 4, "stale")), "");
  assert.equal(replay.event(status(1, exited)), "");
  assert.deepEqual(replay.snapshot(snapshot(1, 0, "stale", exited)), { reset: false, data: "" });
  assert.equal(replay.generation, 2);
  assert.equal(replay.status, running);
  assert.equal(replay.event(output(2, 7, "!")), "!");
});
