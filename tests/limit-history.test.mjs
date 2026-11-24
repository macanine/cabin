import assert from "node:assert/strict";
import test from "node:test";
import { burnGeometry, historyKey, recordSample, samplesFor } from "../src/lib/limitHistory.ts";

const H = 3_600_000;
const memory = () => { const data = new Map(); return { getItem: (k) => data.get(k) ?? null, setItem: (k, v) => data.set(k, v) }; };

test("readings are kept per window, merged when close and dropped after reset", () => {
  const store = memory();
  const now = 1_000_000_000_000;
  const reset = now + 2 * H;
  const key = historyKey("codex:primary", "5 hours", reset);
  assert.equal(key, historyKey("codex:primary", "5 hours", reset + 20_000), "a reset that moves by seconds is the same window");
  recordSample(key, reset, { t: now, used: 10 }, now, store);
  recordSample(key, reset, { t: now + 30_000, used: 11 }, now, store); // within a minute: replaces
  recordSample(key, reset, { t: now + 5 * 60_000, used: 11 }, now, store); // unchanged: skipped
  recordSample(key, reset, { t: now + 20 * 60_000, used: 14 }, now, store);
  recordSample(key, reset, { t: now - 60_000, used: 50 }, now, store); // older than the last: ignored
  assert.deepEqual(samplesFor(key, store), [{ t: now + 30_000, used: 11 }, { t: now + 20 * 60_000, used: 14 }]);

  const later = reset + 1;
  const next = historyKey("codex:primary", "5 hours", later + 5 * H);
  recordSample(next, later + 5 * H, { t: later, used: 1 }, later, store);
  assert.deepEqual(samplesFor(key, store), [], "a finished window is forgotten");
});

test("the burn line opens at zero, passes the readings and projects the pace", () => {
  const now = 1_000_000_000_000;
  const reset = now + H; // 5h window, 4h gone
  const calm = burnGeometry([{ t: now - 2 * H, used: 10 }], { t: now, used: 40 }, reset, 5 * H);
  assert.deepEqual(calm.actual, [[0, 0], [0.4, 0.1], [0.8, 0.4]]);
  assert.deepEqual(calm.projection, [[0.8, 0.4], [1, 0.5]]);
  assert.equal(calm.exhaust, null);

  const hot = burnGeometry([], { t: now - 2 * H, used: 60 }, reset, 5 * H); // 2h gone, 60%: out at 3h20m
  assert.equal(hot.exhaust[1], 1);
  assert.ok(Math.abs(hot.exhaust[0] - 2 / 3) < 1e-9);
  assert.deepEqual(hot.projection.at(-1), [1, 1]);
});
