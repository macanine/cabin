import assert from "node:assert/strict";
import test from "node:test";
import { formatRunTime, gauges, gaugeSeries, toneOf } from "../src/lib/perfGauges.ts";

const GB = 1024 ** 3;
const sample = (patch = {}) => ({
  at: 1, cpuPercent: 23.4, memoryUsed: 6 * GB, memoryTotal: 8 * GB, latencyMs: 20, downloadBps: 0, uploadBps: 0,
  diskReadBps: 0, diskWriteBps: 0, swapUsed: 0, swapTotal: 2 * GB, load: { one: 3.6, five: 2, fifteen: 1 }, cpuCount: 4,
  ...patch,
});

test("four rings: CPU, RAM, swap and load per core", () => {
  const [cpu, ram, swap, load] = gauges(sample());
  assert.deepEqual([cpu.center, cpu.tone], ["23%", "ok"]);
  assert.equal(cpu.detail, "23% of 4 cores");
  assert.deepEqual([ram.percent, ram.center, ram.tone], [75, "75%", "warn"]);
  assert.deepEqual([swap.center, swap.percent], ["0%", 0]);
  assert.deepEqual([load.center, load.percent, load.tone], ["3.6", 90, "bad"]);
  assert.match(load.detail, /3\.60 · 2\.00 · 1\.00/);
});

test("missing values are unknown, not zero", () => {
  const [cpu, ram, swap, load] = gauges(undefined);
  assert.equal(cpu.percent, null);
  assert.equal(ram.tone, "off");
  assert.equal(swap.center, "off");
  assert.equal(load.center, "—");
  assert.equal(gauges(sample({ swapTotal: null, swapUsed: null }))[2].detail, "No swap in the machine");
});

test("tones and helpers", () => {
  assert.deepEqual([toneOf(null), toneOf(69), toneOf(70), toneOf(90)], ["off", "ok", "warn", "bad"]);
  assert.deepEqual(gaugeSeries([sample(), sample({ cpuPercent: null })], "cpu"), [23.4, null]);
  assert.deepEqual([formatRunTime(45), formatRunTime(720), formatRunTime(3 * 3600 + 5 * 60), formatRunTime(98 * 3600)], ["45s", "12m", "3h 05m", "4d 2h"]);
});
