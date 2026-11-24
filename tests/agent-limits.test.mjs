import assert from "node:assert/strict";
import test from "node:test";
import { limitLines, shortLimitLabel } from "../src/lib/agentLimits.ts";

test("limit labels are shortened the same way for every agent", () => {
  assert.equal(shortLimitLabel("5h limit"), "5-hour");
  assert.equal(shortLimitLabel("Current session"), "5-hour");
  assert.equal(shortLimitLabel("7-day"), "Weekly");
  assert.equal(shortLimitLabel("Current week (Opus)"), "Weekly Opus");
  assert.equal(shortLimitLabel("Monthly credits"), "Monthly");
  assert.equal(shortLimitLabel("Daily requests"), "Daily requests");
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

test("forecasts read the pace since the window opened", async () => {
  const { forecastLimit, formatSpan, windowSpanMs } = await import("../src/lib/agentLimits.ts");
  const H = 3_600_000;
  const now = 1_000_000_000_000;
  assert.equal(windowSpanMs({ label: "5 hours" }), 5 * H);
  assert.equal(windowSpanMs({ label: "Current week (Opus)" }), 7 * 24 * H);
  assert.equal(windowSpanMs({ label: "anything", windowMinutes: 300 }), 5 * H);
  assert.equal(formatSpan(45 * 60_000), "45m");
  assert.equal(formatSpan(3 * H + 20 * 60_000), "3h 20m");
  assert.equal(formatSpan(52 * H), "2d 4h");

  // 4 of 5 hours gone, 37% used: on pace for ~46%.
  const calm = forecastLimit({ label: "5 hours", usedPercent: 37, resetsAt: now + H }, now);
  assert.equal(calm.projected, 46);
  assert.equal(calm.tone, "ok");
  assert.equal(calm.text, "~46% used by reset");
  assert.equal(calm.elapsed, 0.8);

  // 2 of 5 hours gone, 60% used: 30%/h runs out in 1h20m, 1h40m before reset.
  const hot = forecastLimit({ label: "5h", usedPercent: 60, resetsAt: now + 3 * H }, now);
  assert.equal(hot.tone, "bad");
  assert.equal(Math.round(hot.runsOutIn / 60_000), 80);
  assert.equal(hot.text, "Runs out in ~1h 20m, 1h 40m before reset");

  assert.equal(forecastLimit({ label: "5h", usedPercent: 20, resetsAt: now + 4.9 * H }, now).text, "Too early to estimate");
  assert.equal(forecastLimit({ label: "5h", usedPercent: 100, resetsAt: now + H }, now).text, "Used up · resets in 1h");
  assert.equal(forecastLimit({ label: "Daily requests", usedPercent: 75, resetsAt: now + H }, now).text, "Resets in 1h");
  assert.equal(forecastLimit({ label: "5h", usedPercent: 50, resetsAt: null }, now).text, "Reset time unknown");
});
