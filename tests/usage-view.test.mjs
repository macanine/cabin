import test from "node:test";
import assert from "node:assert/strict";
import { cacheHitRate, dayBars, formatMetric, modelShares, rangeStart, shortDay } from "../src/lib/usageView.ts";

const model = (name, cost, requests = 1) => ({ model: name, requests, input: 0, output: 0, cacheRead: 0, cacheWrite: 0, cost, unpricedRequests: 0 });

test("day bars fill quiet days with zero and keep the latest", () => {
  const bars = dayBars([{ day: "2026-03-07", n: 2 }, { day: "2026-03-10", n: 5 }], (item) => item.n, "2026-03-07", "2026-03-10");
  assert.deepEqual(bars.map((bar) => [bar.day, bar.value]), [["2026-03-07", 2], ["2026-03-08", 0], ["2026-03-09", 0], ["2026-03-10", 5]]);
  assert.equal(dayBars([], () => 0, "2026-01-01", "2026-12-31", 30).length, 30);
});

test("model shares rank, fold the tail and scale to the largest", () => {
  const rows = modelShares([model("a", 1), model("b", 3), model("c", 0), model("d", 1), model("e", 1)], "cost", 2);
  assert.deepEqual(rows.map((row) => row.key), ["b", "a", "2 other models"]);
  assert.equal(rows[0].share, 1);
  assert.equal(rows[0].percent, 0.5);
});

test("metrics format with suffixes, never the system locale", () => {
  assert.equal(formatMetric(12_400, "tokens"), "12k");
  assert.equal(formatMetric(12.5, "cost"), "$12.50");
  assert.equal(cacheHitRate({ input: 10, output: 5, cacheRead: 30, cacheWrite: 0 }), 0.75);
  assert.equal(cacheHitRate({ input: 0, output: 5, cacheRead: 0, cacheWrite: 0 }), null);
  assert.equal(shortDay("2026-11-02"), "Nov 2");
  assert.equal(rangeStart("all"), null);
  assert.equal(new Date(rangeStart("7", new Date(2026, 10, 20, 15).getTime())).getDate(), 14);
});
