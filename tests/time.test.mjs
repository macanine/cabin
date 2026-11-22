import assert from "node:assert/strict";
import test from "node:test";
import { applyClock } from "../src/lib/clock.ts";
import {
  DAY_MS,
  formatClockTime,
  guestDate,
  isoTimestamp,
  localDayIndex,
  localDayKey,
  localDayKeyFromIndex,
  parseLocalDay,
  weekdayIndex,
} from "../src/lib/time.ts";

test("day keys round-trip through Mac-local midnight", () => {
  const at = new Date(2026, 6, 10, 15, 30).getTime();
  const key = localDayKey(at);
  assert.equal(key, "2026-07-10");
  const start = parseLocalDay(key);
  assert.notEqual(start, null);
  assert.equal(new Date(start).getHours(), 0);
  assert.equal(localDayKey(start + DAY_MS - 1), key);
  assert.equal(parseLocalDay("2026-02-30"), null);
  assert.equal(parseLocalDay("nope"), null);
});

test("day indexes and weekdays are stable in UTC", () => {
  assert.equal(localDayIndex("1970-01-01"), 0);
  assert.equal(localDayIndex("2026-07-10"), Math.floor(Date.UTC(2026, 6, 10) / DAY_MS));
  assert.equal(localDayKeyFromIndex(localDayIndex("2026-07-10")), "2026-07-10");
  assert.equal(weekdayIndex("2026-07-10"), 4); // Friday, Monday = 0
  assert.equal(localDayIndex("bad"), null);
});

test("guest timestamps decode through the measured clock offset", () => {
  const guest = Date.UTC(2026, 6, 10, 12);
  try {
    applyClock({ offset: 3_600_000, rtt: 1 }, null);
    assert.equal(guestDate(guest).getTime(), guest - 3_600_000);
  } finally {
    applyClock({ offset: 0, rtt: 1 }, null);
  }
});

test("absolute timestamps format without applying the guest offset", () => {
  assert.equal(isoTimestamp(Date.UTC(2026, 6, 10, 12)), "2026-07-10T12:00:00.000Z");
  assert.match(formatClockTime(Date.now(), Date.now()), /\d/);
  assert.equal(formatClockTime(0), "");
});
