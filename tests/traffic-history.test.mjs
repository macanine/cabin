import assert from "node:assert/strict";
import test from "node:test";
import { appendTraffic, trafficCurve } from "../src/lib/trafficHistory.ts";

const sample = (second, overrides = {}) => ({
  gatewaySession: 1, sampledAt: second * 1000, rxBytes: second * 50_000,
  txBytes: second * 2500, rxBps: 50_000, txBps: 2500, ...overrides,
});

test("keeps 30 recent one-second samples and starts over across a gap or gateway change", () => {
  let history = [];
  for (let second = 0; second < 40; second++) history = appendTraffic(history, sample(second));
  assert.equal(history.length, 30);
  assert.equal(history[0].sampledAt, 10_000);
  assert.equal(appendTraffic(history, sample(39)), history);
  assert.deepEqual(appendTraffic(history, sample(45)), [sample(45)]);
  const next = sample(40, { gatewaySession: 2, rxBytes: 0, txBytes: 0 });
  assert.deepEqual(appendTraffic(history, next), [next]);
  const reset = sample(40, { rxBytes: 0, txBytes: 0 });
  assert.deepEqual(appendTraffic(history, reset), [reset]);
});

test("scales upload and download independently and draws no idle or one-sample history", () => {
  const history = [sample(0, { rxBps: 0, txBps: 0 }), sample(1)];
  const download = trafficCurve(history, "rxBps");
  const upload = trafficCurve(history, "txBps");
  assert.match(download.line, /100\.0,6\.0$/);
  assert.match(upload.line, /100\.0,29\.6$/);
  assert.match(download.fill, / L100\.0,40 H96\.6 Z$/);
  assert.equal(trafficCurve(history.slice(1), "rxBps"), null);
  assert.equal(trafficCurve([], "rxBps"), null);
  assert.equal(trafficCurve([sample(0, { txBps: 0 }), sample(1, { txBps: 0 })], "txBps"), null);
});
