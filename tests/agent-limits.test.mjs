import assert from "node:assert/strict";
import test from "node:test";
import {
  buildResetTimeline,
  classifyWindowTier,
  FIVE_HOUR_MS,
  forecastLimit,
  formatSpan,
  HOUR_MS,
  limitLines,
  SEVEN_DAY_MS,
  shortLimitLabel,
  windowSpanMs,
} from "../src/lib/agentLimits.ts";

test("limit labels and tiers are normalized identically for every agent", () => {
  assert.equal(shortLimitLabel("5h limit"), "5-hour");
  assert.equal(shortLimitLabel("Current session"), "5-hour");
  assert.equal(shortLimitLabel("7-day"), "Weekly");
  assert.equal(shortLimitLabel("Current week (Opus)"), "Weekly Opus");
  assert.equal(shortLimitLabel("Monthly credits"), "Monthly");
  assert.equal(shortLimitLabel("Daily requests"), "Daily requests");

  assert.equal(classifyWindowTier({ label: "5-hour limit", windowMinutes: 300 }), "5h");
  assert.equal(classifyWindowTier({ label: "Current session" }), "5h");
  assert.equal(classifyWindowTier({ label: "7-day limit", windowMinutes: 10080 }), "7d");
  assert.equal(classifyWindowTier({ label: "Current week (Sonnet)" }), "7d");
  assert.equal(classifyWindowTier({ label: "Monthly credits" }), "other");
});

test("lines say what is left, and flag what is nearly used up", () => {
  assert.deepEqual(limitLines([
    { label: "5h", usedPercent: 6, resetsAt: 1 },
    { label: "Weekly", usedPercent: 93.6, resetsAt: null },
    { label: "7-day", usedPercent: 120, resetsAt: null },
  ]), [
    { text: "5-hour 94% left", low: false, resetsAt: 1 },
    { text: "Weekly 6% left", low: true, resetsAt: null },
    { text: "Weekly used up", low: true, resetsAt: null },
  ]);
});

test("forecasts compute pace ratio, even-pace delta, sustainable rate, and projections", () => {
  const now = 1_000_000_000_000;
  assert.equal(windowSpanMs({ label: "5 hours" }), FIVE_HOUR_MS);
  assert.equal(windowSpanMs({ label: "Current week (Opus)" }), SEVEN_DAY_MS);
  assert.equal(windowSpanMs({ label: "anything", windowMinutes: 300 }), FIVE_HOUR_MS);
  assert.equal(formatSpan(45 * 60_000), "45m");
  assert.equal(formatSpan(3 * HOUR_MS + 20 * 60_000), "3h 20m");
  assert.equal(formatSpan(52 * HOUR_MS), "2d 4h");

  // 4 of 5 hours gone (80% elapsed), 37% used: under even pace (0.46x), 63% left for 1h.
  const calm = forecastLimit({ label: "5 hours", usedPercent: 37, resetsAt: now + HOUR_MS }, now);
  assert.equal(calm.projected, 46);
  assert.equal(calm.tone, "ok");
  assert.equal(calm.status, "under");
  assert.equal(calm.left, 63);
  assert.equal(calm.paceRatio, 0.46);
  assert.equal(calm.deltaFromEven, -43);
  assert.equal(calm.sustainableRate?.unit, "h");
  assert.equal(Math.round(calm.sustainableRate.value), 63);
  assert.equal(calm.text, "~46% used by reset");
  assert.equal(calm.elapsed, 0.8);

  // 2 of 5 hours gone (40% elapsed), 60% used: 1.5x fast pace -> runs out in 1h20m.
  const hot = forecastLimit({ label: "5h", usedPercent: 60, resetsAt: now + 3 * HOUR_MS }, now);
  assert.equal(hot.tone, "bad");
  assert.equal(hot.status, "fast");
  assert.equal(hot.paceRatio, 1.5);
  assert.equal(hot.deltaFromEven, 20);
  assert.equal(Math.round(hot.runsOutIn / 60_000), 80);
  assert.equal(hot.text, "Runs out in ~1h 20m, 1h 40m before reset");

  // Weekly window reports sustainable rate per day.
  const weekly = forecastLimit({ label: "7-day", usedPercent: 30, resetsAt: now + 5 * 24 * HOUR_MS }, now);
  assert.equal(weekly.sustainableRate?.unit, "d");
  assert.equal(Math.round(weekly.sustainableRate.value), 14);

  assert.equal(forecastLimit({ label: "5h", usedPercent: 20, resetsAt: now + 4.9 * HOUR_MS }, now).text, "Too early to estimate");
  assert.equal(forecastLimit({ label: "5h", usedPercent: 100, resetsAt: now + HOUR_MS }, now).text, "Used up · resets in 1h");
  assert.equal(forecastLimit({ label: "Daily requests", usedPercent: 75, resetsAt: now + HOUR_MS }, now).text, "Resets in 1h");
  assert.equal(forecastLimit({ label: "5h", usedPercent: 50, resetsAt: null }, now).text, "Reset time unknown");
});

test("Magpie reset timeline sorts by soonest reset and clusters concurrent windows", () => {
  const now = 1_000_000_000_000;
  const windows = [
    { id: "late", resetsAt: now + 4 * HOUR_MS },
    { id: "soon-a", resetsAt: now + HOUR_MS },
    { id: "soon-b", resetsAt: now + HOUR_MS + 4 * 60_000 },
    { id: "past", resetsAt: now - 1000 },
    { id: "outside", resetsAt: now + 10 * HOUR_MS },
    { id: "unknown", resetsAt: null },
  ];

  const clusters = buildResetTimeline(windows, FIVE_HOUR_MS, now, { clusterWindowMs: 10 * 60_000 });
  assert.equal(clusters.length, 2);
  assert.deepEqual(clusters[0].entries.map((e) => e.item.id), ["soon-a", "soon-b"]);
  assert.ok(Math.abs(clusters[0].position - 0.2) < 1e-6);
  assert.deepEqual(clusters[1].entries.map((e) => e.item.id), ["late"]);
  assert.ok(Math.abs(clusters[1].position - 0.8) < 1e-6);
});
