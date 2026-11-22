import test from "node:test";
import assert from "node:assert/strict";
import {
  isAttach,
  logTicker,
  remainingPhases,
  startProgress,
  visiblePhases,
} from "../src/lib/startReport.ts";

const phase = (id, state) => ({ id, title: id, state, startedAt: null, finishedAt: null, detail: null });
const report = (state, phases) => ({
  state,
  startedAt: 0,
  finishedAt: null,
  error: null,
  facts: [],
  phases,
  logPath: null,
  logTail: [],
  serialPath: null,
  serialTail: [],
});

test("only begun phases are shown; skipped ones stay hidden", () => {
  const r = report("running", [phase("gateway", "done"), phase("boot", "skipped"), phase("daemon", "running"), phase("library", "pending")]);
  assert.deepEqual(visiblePhases(r).map((p) => p.id), ["gateway", "daemon"]);
  assert.equal(remainingPhases(r), 1);
  assert.equal(isAttach(r), true);
});

test("progress counts settled phases fully and the running one half", () => {
  const r = report("running", [phase("a", "done"), phase("b", "skipped"), phase("c", "running"), phase("d", "pending")]);
  assert.equal(startProgress(r), 2.5 / 4);
  assert.equal(startProgress({ ...r, state: "succeeded" }), 1);
  assert.equal(remainingPhases({ ...r, state: "failed" }), 0);
});

test("the ticker is the newest non-empty line without its elapsed prefix", () => {
  assert.equal(logTicker(["[+0.001s] Startup attempt began", "[+1.250s] Connecting to cabind", "  "]), "Connecting to cabind");
  assert.equal(logTicker([]), null);
  assert.equal(logTicker(["x".repeat(200)]).length, 140);
});
