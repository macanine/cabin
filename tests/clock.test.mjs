import assert from "node:assert/strict";
import test from "node:test";
import {
  applyClock, clockNote, clockState, estimateOffset, guestNow, hostTime, MAX_RTT_MS, nextOffset,
} from "../src/lib/clock.ts";
import { lastActivity, shortAge } from "../src/lib/taskList.ts";

test("the offset comes from the fastest round trip, read at its midpoint", () => {
  // Guest 5 min behind; the second sample has the shortest round trip.
  const behind = -300_000;
  const t = 1_800_000_000_000;
  const estimate = estimateOffset([
    { sent: t + 1000, guest: t + 1000 + 400 + behind, received: t + 1800 },
    { sent: t + 2000, guest: t + 2000 + 20 + behind, received: t + 2040 },
    { sent: t + 3000, guest: t + 3000 + 90 + behind, received: t + 3300 },
  ]);
  assert.deepEqual(estimate, { offset: behind, rtt: 40 });
});

test("unusable samples give no estimate", () => {
  assert.equal(estimateOffset([]), null);
  assert.equal(estimateOffset([{ sent: 0, guest: 5, received: MAX_RTT_MS + 1 }]), null);
  assert.equal(estimateOffset([{ sent: 10, guest: 5, received: 5 }]), null);
  assert.equal(estimateOffset([{ sent: 10, guest: 0, received: 20 }]), null);
});

test("small changes are noise; real jumps are taken", () => {
  assert.equal(nextOffset(0, { offset: 300, rtt: 20 }), 0);
  assert.equal(nextOffset(0, { offset: 900, rtt: 20 }), 900);
  // A slow sample needs a bigger change to count.
  assert.equal(nextOffset(0, { offset: 900, rtt: 2400 }), 0);
  assert.equal(nextOffset(-60_000, null), -60_000);
});

test("guest timestamps land on the Mac's clock, and back", (t) => {
  const macNow = 1_800_000_000_000;
  t.mock.method(Date, "now", () => macNow);
  applyClock({ offset: -120_000, rtt: 10 }, "Asia/Tokyo", 0);
  assert.equal(clockState().offset, -120_000);
  assert.equal(clockState().guestZone, "Asia/Tokyo");
  assert.equal(hostTime(macNow - 120_000), macNow);
  assert.equal(guestNow(), macNow - 120_000);
  // A session the guest touched just now is "now", not "2m" old.
  const session = { createdAt: guestNow(), updatedAt: guestNow(), lastStartedAt: null };
  assert.equal(shortAge(lastActivity(session)), "now");
  // A failed measurement keeps the last offset and zone.
  applyClock(null, null);
  assert.equal(clockState().offset, -120_000);
  assert.equal(clockState().guestZone, "Asia/Tokyo");
  applyClock({ offset: 0, rtt: 10 }, null);
});

test("the note explains a different zone and a drifting clock, and stays quiet otherwise", () => {
  assert.equal(clockNote({ offset: 0, guestZone: "Europe/Berlin" }, "Europe/Berlin"), null);
  assert.equal(clockNote({ offset: 30_000, guestZone: null }, "Europe/Berlin"), null);
  assert.match(clockNote({ offset: 0, guestZone: "Asia/Tokyo" }, "Europe/Berlin"), /Asia\/Tokyo.*Europe\/Berlin.*this Mac's zone/);
  assert.match(clockNote({ offset: -180_000, guestZone: null }, null), /3 min behind/);
  assert.match(clockNote({ offset: 90_000, guestZone: null }, null), /90 s ahead of/);
});
