import assert from "node:assert/strict";
import test from "node:test";
import { formatBytes, formatCount, formatInteger, formatMs, formatPrice, formatRate, formatUsd, relativeTime, sparkline } from "../src/lib/format.ts";

test("missing values render as a dash, never a fake number", () => {
  assert.equal(formatBytes(null), "—");
  assert.equal(formatRate(undefined), "—");
  assert.equal(formatMs(null), "—");
});

test("bytes and rates", () => {
  assert.equal(formatBytes(512), "512 B");
  assert.equal(formatBytes(1536), "1.5 KB");
  assert.equal(formatRate(10 * 1024 * 1024), "10 MB/s");
});

test("relative time", () => {
  assert.equal(relativeTime(1000, 1000), "just now");
  assert.equal(relativeTime(0, 5 * 60 * 1000), "5 min ago");
});

test("sparkline skips gaps and needs two points", () => {
  assert.equal(sparkline([null, 5], 100, 10), "");
  assert.equal(sparkline([0, 10], 100, 10, 10), "0.0,10.0 100.0,0.0");
});

test("download summary", async () => {
  const { downloadSummary } = await import("../src/lib/format.ts");
  const mb = 1024 * 1024;
  assert.deepEqual(downloadSummary({ doneBytes: 312 * mb, totalBytes: 610 * mb, bytesPerSec: 12 * mb, verifying: false }), {
    percent: 51,
    text: "312 MB of 610 MB · 12 MB/s · about 25s left",
  });
  assert.deepEqual(downloadSummary({ doneBytes: 5 * mb, totalBytes: null, bytesPerSec: null, verifying: false }), { percent: null, text: "5.0 MB" });
  assert.equal(downloadSummary({ doneBytes: 9, totalBytes: 9, bytesPerSec: 1, verifying: true }).percent, 100);
});

test("counts use k, M, B, T, never the system locale", () => {
  assert.equal(formatCount(950), "950");
  assert.equal(formatCount(1234), "1.2k");
  assert.equal(formatCount(999_960), "1M");
  assert.equal(formatCount(1_250_000), "1.25M");
  assert.equal(formatCount(34_500_000), "34.5M");
  assert.equal(formatCount(3_400_000_000), "3.4B");
  assert.equal(formatCount(2e12), "2T");
  assert.equal(formatCount(null), "—");
  assert.equal(formatInteger(1234567), "1,234,567");
});

test("money and per-token prices", () => {
  assert.equal(formatUsd(0), "$0");
  assert.equal(formatUsd(0.004), "<$0.01");
  assert.equal(formatUsd(12.345), "$12.35");
  assert.equal(formatUsd(1234.5), "$1,235");
  assert.equal(formatUsd(12_400), "$12k");
  assert.equal(formatPrice(3), "$3");
  assert.equal(formatPrice(0.075), "$0.075");
  assert.equal(formatPrice(15.5), "$15.5");
});
